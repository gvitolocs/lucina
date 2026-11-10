//! 802.11 frame headers and the fixed management frames (`include/linux/ieee80211.h`, `net/mac80211/util.c`).

use alloc::vec::Vec;

pub type MacAddr = [u8; 6];

pub const BROADCAST: MacAddr = [0xff; 6];

/// Frame control, low byte: protocol version (0), type, subtype.
pub const FTYPE_MGMT: u16 = 0x0000;
pub const FTYPE_DATA: u16 = 0x0008;
pub const FTYPE_MASK: u16 = 0x000c;
pub const STYPE_MASK: u16 = 0x00f0;

pub const STYPE_ASSOC_REQ: u16 = 0x0000;
pub const STYPE_ASSOC_RESP: u16 = 0x0010;
pub const STYPE_REASSOC_RESP: u16 = 0x0030;
pub const STYPE_PROBE_REQ: u16 = 0x0040;
pub const STYPE_PROBE_RESP: u16 = 0x0050;
pub const STYPE_BEACON: u16 = 0x0080;
pub const STYPE_DISASSOC: u16 = 0x00a0;
pub const STYPE_AUTH: u16 = 0x00b0;
pub const STYPE_DEAUTH: u16 = 0x00c0;

/// Data subtype bits: QoS, and "no data" for the null functions.
pub const STYPE_QOS_BIT: u16 = 0x0080;
pub const STYPE_NODATA_BIT: u16 = 0x0040;

pub const FCTL_TODS: u16 = 0x0100;
pub const FCTL_FROMDS: u16 = 0x0200;
pub const FCTL_MOREFRAGS: u16 = 0x0400;
pub const FCTL_PROTECTED: u16 = 0x4000;
pub const FCTL_ORDER: u16 = 0x8000;

pub const CAP_ESS: u16 = 0x0001;
pub const CAP_IBSS: u16 = 0x0002;
pub const CAP_PRIVACY: u16 = 0x0010;
pub const CAP_SHORT_PREAMBLE: u16 = 0x0020;
pub const CAP_SPECTRUM_MGMT: u16 = 0x0100;
pub const CAP_SHORT_SLOT_TIME: u16 = 0x0400;

pub const WLAN_AUTH_OPEN: u16 = 0;

pub const STATUS_SUCCESS: u16 = 0;
/// Rejected temporarily; a Timeout Interval element says when to come back.
pub const STATUS_ASSOC_REJECTED_TEMPORARILY: u16 = 30;

pub const REASON_UNSPECIFIED: u16 = 1;
pub const REASON_DEAUTH_LEAVING: u16 = 3;
pub const REASON_DISASSOC_DUE_TO_INACTIVITY: u16 = 4;
pub const REASON_MICHAEL_MIC_FAILURE: u16 = 14;
pub const REASON_4WAY_HANDSHAKE_TIMEOUT: u16 = 15;
pub const REASON_IE_DIFFERENT: u16 = 17;

/// The first three octets of an RFC 1042 LLC/SNAP header with OUI 0, and of bridge-tunnel (OUI 00-00-F8).
pub const RFC1042_HEADER: [u8; 6] = [0xaa, 0xaa, 0x03, 0x00, 0x00, 0x00];
pub const BRIDGE_TUNNEL_HEADER: [u8; 6] = [0xaa, 0xaa, 0x03, 0x00, 0x00, 0xf8];

pub const ETH_P_EAPOL: u16 = 0x888e;
pub const ETH_P_AARP: u16 = 0x80f3;
pub const ETH_P_IPX: u16 = 0x8137;
pub const ETH_P_802_3_MIN: u16 = 0x0600;
pub const ETH_ALEN: usize = 6;
pub const ETH_HLEN: usize = 14;

/// True for multicast and broadcast addresses.
pub fn is_multicast(addr: &MacAddr) -> bool {
    addr[0] & 1 != 0
}

pub(crate) fn mac_at(b: &[u8], offset: usize) -> Option<MacAddr> {
    let s = b.get(offset..offset + 6)?;
    let mut m = [0; 6];
    m.copy_from_slice(s);
    Some(m)
}

pub(crate) fn le16_at(b: &[u8], offset: usize) -> Option<u16> {
    let s = b.get(offset..offset + 2)?;
    Some(u16::from_le_bytes([s[0], s[1]]))
}

pub(crate) fn be16_at(b: &[u8], offset: usize) -> Option<u16> {
    let s = b.get(offset..offset + 2)?;
    Some(u16::from_be_bytes([s[0], s[1]]))
}

/// `ieee80211_hdrlen()` for management and data frames: `None` for control and extension frames.
pub fn hdrlen(fc: u16) -> Option<usize> {
    match fc & FTYPE_MASK {
        FTYPE_MGMT => Some(if fc & FCTL_ORDER != 0 { 28 } else { 24 }),
        FTYPE_DATA => {
            let mut len = if fc & (FCTL_TODS | FCTL_FROMDS) == FCTL_TODS | FCTL_FROMDS {
                30
            } else {
                24
            };
            if fc & STYPE_QOS_BIT != 0 {
                len += 2;
                if fc & FCTL_ORDER != 0 {
                    len += 4;
                }
            }
            Some(len)
        }
        _ => None,
    }
}

/// A management frame's header, and its body as a slice of the same frame.
#[derive(Clone, Copy, Debug)]
pub struct MgmtFrame<'a> {
    pub fc: u16,
    pub da: MacAddr,
    pub sa: MacAddr,
    pub bssid: MacAddr,
    pub body: &'a [u8],
}

impl<'a> MgmtFrame<'a> {
    pub fn parse(frame: &'a [u8]) -> Option<Self> {
        let fc = le16_at(frame, 0)?;
        if fc & FTYPE_MASK != FTYPE_MGMT || fc & 3 != 0 {
            return None;
        }
        let len = hdrlen(fc)?;
        Some(Self {
            fc,
            da: mac_at(frame, 4)?,
            sa: mac_at(frame, 10)?,
            bssid: mac_at(frame, 16)?,
            body: frame.get(len..)?,
        })
    }

    pub fn subtype(&self) -> u16 {
        self.fc & STYPE_MASK
    }
}

/// Starts a management frame with sequence control 0. The station writes the sequence number when it queues it.
pub fn mgmt_header(
    subtype: u16,
    da: &MacAddr,
    sa: &MacAddr,
    bssid: &MacAddr,
    capacity: usize,
) -> Vec<u8> {
    let mut f = Vec::with_capacity(24 + capacity);
    f.extend_from_slice(&(FTYPE_MGMT | subtype).to_le_bytes());
    f.extend_from_slice(&[0, 0]);
    f.extend_from_slice(da);
    f.extend_from_slice(sa);
    f.extend_from_slice(bssid);
    f.extend_from_slice(&[0, 0]);
    f
}

/// Writes the sequence number (fragment 0) into a management or 3-address data frame.
pub fn set_sequence(frame: &mut [u8], seq: u16) {
    if let Some(s) = frame.get_mut(22..24) {
        s.copy_from_slice(&((seq & 0x0fff) << 4).to_le_bytes());
    }
}

/// Authentication frame, open system, transaction 1, as `ieee80211_send_auth()` builds it.
pub fn build_auth(ap: &MacAddr, own: &MacAddr) -> Vec<u8> {
    let mut f = mgmt_header(STYPE_AUTH, ap, own, ap, 6);
    f.extend_from_slice(&WLAN_AUTH_OPEN.to_le_bytes());
    f.extend_from_slice(&1u16.to_le_bytes());
    f.extend_from_slice(&STATUS_SUCCESS.to_le_bytes());
    f
}

/// Deauthentication with a reason code.
pub fn build_deauth(ap: &MacAddr, own: &MacAddr, reason: u16) -> Vec<u8> {
    let mut f = mgmt_header(STYPE_DEAUTH, ap, own, ap, 2);
    f.extend_from_slice(&reason.to_le_bytes());
    f
}

/// Fields of a received Authentication frame's fixed part.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuthBody {
    pub algorithm: u16,
    pub transaction: u16,
    pub status: u16,
}

impl AuthBody {
    pub fn parse(body: &[u8]) -> Option<Self> {
        Some(Self {
            algorithm: le16_at(body, 0)?,
            transaction: le16_at(body, 2)?,
            status: le16_at(body, 4)?,
        })
    }
}
