//! The data path's frame conversions: Ethernet II to and from ath10k's native-WiFi format, raw 802.11 with QoS to
//! Ethernet, and A-MSDU splitting. From `ieee80211_build_hdr`, `ieee80211_data_to_8023_exthdr`,
//! `ieee80211_amsdu_to_8023s`, `ath10k_htt_rx_h_undecap_nwifi` and `ath10k_tx_h_nwifi`.

use crate::frame::{
    BRIDGE_TUNNEL_HEADER, ETH_ALEN, ETH_HLEN, ETH_P_802_3_MIN, ETH_P_AARP, ETH_P_IPX, FCTL_FROMDS,
    FCTL_PROTECTED, FCTL_TODS, FTYPE_DATA, FTYPE_MASK, MacAddr, RFC1042_HEADER, STYPE_NODATA_BIT,
    STYPE_QOS_BIT, be16_at, hdrlen, is_multicast, le16_at, mac_at,
};
use alloc::vec::Vec;

/// Bit 7 of the QoS Control field: the payload is an A-MSDU.
const QOS_CTL_A_MSDU_PRESENT: u8 = 0x80;

/// Ethernet II to a native-WiFi frame for transmit: 3-address To-DS header, no QoS, LLC/SNAP, payload. ath10k takes
/// this form with `ATH10K_HW_TXRX_NATIVE_WIFI`; the QoS field is for firmware to add. `sa` is the station's own
/// address, which is what Address 2 must be. `protected` tells the firmware to encrypt it, which mac80211 expresses
/// through the Protected bit. `None` if the frame is shorter than an Ethernet header.
pub fn ethernet_to_nwifi(
    eth: &[u8],
    bssid: &MacAddr,
    sa: &MacAddr,
    protected: bool,
) -> Option<Vec<u8>> {
    let da = mac_at(eth, 0)?;
    let ethertype = be16_at(eth, 12)?;
    let payload = eth.get(ETH_HLEN..)?;

    let mut fc = FTYPE_DATA | FCTL_TODS;
    if protected {
        fc |= FCTL_PROTECTED;
    }
    let mut f = Vec::with_capacity(24 + 8 + payload.len());
    f.extend_from_slice(&fc.to_le_bytes());
    f.extend_from_slice(&[0, 0]);
    f.extend_from_slice(bssid);
    f.extend_from_slice(sa);
    f.extend_from_slice(&da);
    f.extend_from_slice(&[0, 0]);
    // An 802.3 frame (length, not type) already carries its LLC.
    if ethertype >= ETH_P_802_3_MIN {
        if ethertype == ETH_P_AARP || ethertype == ETH_P_IPX {
            f.extend_from_slice(&BRIDGE_TUNNEL_HEADER);
        } else {
            f.extend_from_slice(&RFC1042_HEADER);
        }
        f.extend_from_slice(&ethertype.to_be_bytes());
    }
    f.extend_from_slice(payload);
    Some(f)
}

/// A data frame's addressing, from `ieee80211_get_DA` and `ieee80211_get_SA`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DataHeader {
    pub fc: u16,
    pub da: MacAddr,
    pub sa: MacAddr,
    /// Address 2, the transmitter: the AP on anything a station should accept.
    pub ta: MacAddr,
    /// Header length, QoS Control included.
    pub len: usize,
    /// QoS Control's low byte, for QoS frames.
    pub qos: Option<u8>,
}

impl DataHeader {
    pub fn parse(frame: &[u8]) -> Option<Self> {
        let fc = le16_at(frame, 0)?;
        if fc & FTYPE_MASK != FTYPE_DATA || fc & 3 != 0 || fc & STYPE_NODATA_BIT != 0 {
            return None;
        }
        let len = hdrlen(fc)?;
        if frame.len() < len {
            return None;
        }
        let a1 = mac_at(frame, 4)?;
        let a2 = mac_at(frame, 10)?;
        let a3 = mac_at(frame, 16)?;
        let tods = fc & FCTL_TODS != 0;
        let fromds = fc & FCTL_FROMDS != 0;
        let da = if tods { a3 } else { a1 };
        let sa = match (tods, fromds) {
            (false, true) => a3,
            (true, true) => mac_at(frame, 24)?,
            _ => a2,
        };
        let qos = if fc & STYPE_QOS_BIT != 0 {
            // QoS Control follows the addresses: 24, or 30 with a fourth one. An Order bit's HT Control comes after.
            Some(*frame.get(if tods && fromds { 30 } else { 24 })?)
        } else {
            None
        };
        Some(Self {
            fc,
            da,
            sa,
            ta: a2,
            len,
            qos,
        })
    }
}

/// `ieee80211_get_8023_tunnel_proto()`: the ethertype, if this LLC/SNAP header is one a bridge would translate.
fn tunnel_proto(llc: &[u8]) -> Option<u16> {
    let oui = llc.get(..ETH_ALEN)?;
    let proto = be16_at(llc, ETH_ALEN)?;
    if oui == RFC1042_HEADER && proto != ETH_P_AARP && proto != ETH_P_IPX {
        return Some(proto);
    }
    (oui == BRIDGE_TUNNEL_HEADER).then_some(proto)
}

/// One MSDU's bytes, with their addresses, to Ethernet: strips a translated LLC/SNAP, or else makes an 802.3 frame
/// with the length in the type field.
fn msdu_to_ethernet(da: &MacAddr, sa: &MacAddr, msdu: &[u8]) -> Vec<u8> {
    let (proto, payload) = match tunnel_proto(msdu) {
        Some(p) => (p, &msdu[ETH_ALEN + 2..]),
        None => (msdu.len() as u16, msdu),
    };
    let mut e = Vec::with_capacity(ETH_HLEN + payload.len());
    e.extend_from_slice(da);
    e.extend_from_slice(sa);
    e.extend_from_slice(&proto.to_be_bytes());
    e.extend_from_slice(payload);
    e
}

/// A native-WiFi frame from `ath10k_htt_rx_h_undecap_nwifi`'s input, or any non-A-MSDU data frame with the crypto
/// header gone, to Ethernet II. Native-WiFi has no A-MSDU subframe headers even inside an A-MSDU: each MSDU is
/// delivered separately with its own addresses.
pub fn nwifi_to_ethernet(frame: &[u8]) -> Option<Vec<u8>> {
    let h = DataHeader::parse(frame)?;
    Some(msdu_to_ethernet(&h.da, &h.sa, frame.get(h.len..)?))
}

/// A raw 802.11 data frame to Ethernet: `crypto_header` is the length of any IV still present (0 if stripped, 8 for
/// CCMP left in). The MIC and FCS must already be removed. Splits an A-MSDU with [`split_amsdu`]. Empty if the frame
/// is unusable, as `ieee80211_rx_h_amsdu` drops a whole bad A-MSDU.
pub fn raw80211_to_ethernet(frame: &[u8], crypto_header: usize) -> Vec<Vec<u8>> {
    let Some(h) = DataHeader::parse(frame) else {
        return Vec::new();
    };
    let Some(body) = frame.get(h.len + crypto_header..) else {
        return Vec::new();
    };
    if h.qos.is_some_and(|q| q & QOS_CTL_A_MSDU_PRESENT != 0) {
        // A station takes the outer DA and does not filter on SA (`__ieee80211_rx_h_amsdu`).
        split_amsdu(body, Some(&h.da), None)
    } else {
        Vec::from([msdu_to_ethernet(&h.da, &h.sa, body)])
    }
}

/// `ieee80211_amsdu_to_8023s()`: splits an A-MSDU payload into Ethernet frames. Subframes for another unicast address
/// (`check_da`) or another source (`check_sa`) are skipped. A malformed A-MSDU gives nothing at all, and so does one
/// that opens with a subframe whose DA looks like an LLC header: `is_amsdu_aggregation_attack`, the plaintext
/// aggregation attack (FragAttacks) where an attacker flips the A-MSDU bit on a normal frame.
pub fn split_amsdu(
    payload: &[u8],
    check_da: Option<&MacAddr>,
    check_sa: Option<&MacAddr>,
) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut offset = 0;
    let mut last = false;
    while !last {
        let remaining = payload.len() - offset;
        if remaining < ETH_HLEN {
            return Vec::new();
        }
        let da = mac_at(payload, offset).unwrap_or_default();
        let sa = mac_at(payload, offset + ETH_ALEN).unwrap_or_default();
        let len = be16_at(payload, offset + 2 * ETH_ALEN).unwrap_or(0) as usize;
        let subframe = ETH_HLEN + len;
        let padding = subframe.wrapping_neg() & 3;
        if subframe > remaining {
            return Vec::new();
        }
        if offset == 0 && da == RFC1042_HEADER {
            return Vec::new();
        }
        last = remaining <= subframe + padding;
        let wanted = (is_multicast(&da) || check_da.is_none_or(|c| *c == da))
            && check_sa.is_none_or(|c| *c == sa);
        if wanted {
            let msdu = &payload[offset + ETH_HLEN..offset + subframe];
            out.push(msdu_to_ethernet(&da, &sa, msdu));
        }
        offset += subframe + padding;
    }
    out
}

/// The ethertype of an Ethernet II frame.
pub fn ethertype(eth: &[u8]) -> Option<u16> {
    be16_at(eth, 12)
}
