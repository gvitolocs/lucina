//! CDC-ECM: class requests, notifications, the MAC address string and bulk framing.
//!
//! An ECM data interface carries one Ethernet frame (no FCS) per bulk transfer in each direction (ECM 1.2
//! section 3.3.1). A transfer ends with a short packet; a frame whose length is a multiple of the endpoint's
//! maximum packet size therefore needs an end marker. The specification asks for a zero-length packet; Linux
//! (`usbnet_start_xmit` without `FLAG_SEND_ZLP`, which cdc_ether does not set) instead appends one zero byte, so
//! that the transfer ends with a short packet. [`tx_len`] does the same: some devices mishandle zero-length
//! packets, and the extra byte is harmless Ethernet padding. On reception a zero-length transfer carries no frame.

/// Class-specific requests (CDC 1.2 table 19, ECM 1.2 table 6), all to the control interface.
pub const SET_ETHERNET_MULTICAST_FILTERS: u8 = 0x40;
pub const SET_ETHERNET_PACKET_FILTER: u8 = 0x43;
pub const GET_ETHERNET_STATISTIC: u8 = 0x44;

/// `SET_ETHERNET_PACKET_FILTER` bits (ECM 1.2 table 8).
pub const PACKET_TYPE_PROMISCUOUS: u16 = 1 << 0;
pub const PACKET_TYPE_ALL_MULTICAST: u16 = 1 << 1;
pub const PACKET_TYPE_DIRECTED: u16 = 1 << 2;
pub const PACKET_TYPE_BROADCAST: u16 = 1 << 3;
pub const PACKET_TYPE_MULTICAST: u16 = 1 << 4;

/// The filter Linux sets when the interface comes up without promiscuous or all-multicast mode
/// (`usbnet_cdc_update_filter`): directed, broadcast and multicast frames that pass the device's filters.
pub const DEFAULT_PACKET_FILTER: u16 =
    PACKET_TYPE_DIRECTED | PACKET_TYPE_BROADCAST | PACKET_TYPE_MULTICAST;

/// Notification codes (CDC 1.2 table 20).
pub const NOTIFY_NETWORK_CONNECTION: u8 = 0x00;
pub const NOTIFY_RESPONSE_AVAILABLE: u8 = 0x01;
pub const NOTIFY_CONNECTION_SPEED_CHANGE: u8 = 0x2a;

/// bmRequestType of a notification: device to host, class, interface.
const NOTIFY_REQUEST_TYPE: u8 = 0xa1;

/// A notification on the interrupt endpoint, as far as an Ethernet driver acts on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Notification {
    /// NETWORK_CONNECTION: the link went up (`true`) or down.
    Connection(bool),
    /// CONNECTION_SPEED_CHANGE: downstream (receive) and upstream (transmit) bit rates.
    Speed { downstream: u32, upstream: u32 },
    /// Any other well-formed notification (e.g. RESPONSE_AVAILABLE), ignored as Linux's `usbnet_cdc_status` does.
    Other(u8),
}

/// Parses one notification. Returns `None` for a transfer too short for its header or its data, or one that is not
/// a class notification; Linux logs and drops those too.
pub fn parse_notification(buf: &[u8]) -> Option<Notification> {
    if buf.len() < 8 || buf[0] != NOTIFY_REQUEST_TYPE {
        return None;
    }
    let value = u16::from_le_bytes([buf[2], buf[3]]);
    match buf[1] {
        NOTIFY_NETWORK_CONNECTION => Some(Notification::Connection(value != 0)),
        NOTIFY_CONNECTION_SPEED_CHANGE => {
            // wLength is 8: two le32 bit rates follow the header.
            if buf.len() < 16 {
                return None;
            }
            let word =
                |at: usize| u32::from_le_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]]);
            Some(Notification::Speed {
                downstream: word(8),
                upstream: word(12),
            })
        }
        code => Some(Notification::Other(code)),
    }
}

/// Parses the iMACAddress string: 12 hexadecimal digits, most significant first (ECM 1.2 table 3, Linux
/// `usbnet_get_ethernet_addr`). Returns `None` for anything else; Linux then refuses the device.
pub fn parse_mac(text: &str) -> Option<[u8; 6]> {
    let digits = text.trim_end_matches('\0').as_bytes();
    if digits.len() != 12 {
        return None;
    }
    let nibble = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    let mut mac = [0u8; 6];
    for (i, byte) in mac.iter_mut().enumerate() {
        *byte = nibble(digits[2 * i])? << 4 | nibble(digits[2 * i + 1])?;
    }
    Some(mac)
}

/// Whether `mac` can be an interface address: not multicast and not all zeros.
pub fn is_valid_unicast(mac: &[u8; 6]) -> bool {
    mac[0] & 1 == 0 && mac.iter().any(|&b| b != 0)
}

/// The bulk OUT transfer length for a frame of `frame_len` bytes: one more (a zero pad byte) when the frame is a
/// multiple of `max_packet`, see the module documentation.
pub fn tx_len(frame_len: usize, max_packet: u16) -> usize {
    if max_packet != 0 && frame_len.is_multiple_of(max_packet as usize) {
        frame_len + 1
    } else {
        frame_len
    }
}

/// Whether a received transfer of `len` bytes is a frame to hand up: at least an Ethernet header (Linux's
/// `usbnet` drops runts) and no longer than the device's maximum segment size plus one VLAN tag, which
/// `wMaxSegmentSize` does not count.
pub fn rx_frame_ok(len: usize, max_segment: usize) -> bool {
    len >= crate::ETH_HLEN && len <= max_segment + 4
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qemu_connection_notification() {
        // hw/usb/dev-network.c usb_net_handle_statusin in CDC mode: A1 00, wValue = connection, wIndex 1.
        let buf = [0xa1, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00];
        assert_eq!(
            parse_notification(&buf),
            Some(Notification::Connection(true))
        );
        let down = [0xa1, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        assert_eq!(
            parse_notification(&down),
            Some(Notification::Connection(false))
        );
    }

    #[test]
    fn speed_notification() {
        let mut buf = [0u8; 16];
        buf[..8].copy_from_slice(&[0xa1, 0x2a, 0, 0, 0, 0, 8, 0]);
        buf[8..12].copy_from_slice(&100_000_000u32.to_le_bytes());
        buf[12..16].copy_from_slice(&10_000_000u32.to_le_bytes());
        assert_eq!(
            parse_notification(&buf),
            Some(Notification::Speed {
                downstream: 100_000_000,
                upstream: 10_000_000
            })
        );
        assert_eq!(parse_notification(&buf[..8]), None);
    }

    #[test]
    fn other_and_bad_notifications() {
        assert_eq!(
            parse_notification(&[0xa1, 0x01, 0, 0, 0, 0, 0, 0]),
            Some(Notification::Other(NOTIFY_RESPONSE_AVAILABLE))
        );
        assert_eq!(parse_notification(&[0xa1, 0x00, 1, 0]), None);
        // The RNDIS "response available" word that QEMU sends in its other configuration.
        assert_eq!(parse_notification(&[1, 0, 0, 0, 0, 0, 0, 0]), None);
    }

    #[test]
    fn mac_strings() {
        assert_eq!(
            parse_mac("525400123456"),
            Some([0x52, 0x54, 0x00, 0x12, 0x34, 0x56])
        );
        assert_eq!(
            parse_mac("02AbCdEf0102"),
            Some([0x02, 0xab, 0xcd, 0xef, 0x01, 0x02])
        );
        assert_eq!(parse_mac("52540012345"), None);
        assert_eq!(parse_mac("5254001234567"), None);
        assert_eq!(parse_mac("52540012345g"), None);
        assert!(is_valid_unicast(&[0x52, 0x54, 0, 0x12, 0x34, 0x56]));
        assert!(!is_valid_unicast(&[0x01, 0, 0x5e, 0, 0, 1]));
        assert!(!is_valid_unicast(&[0; 6]));
    }

    #[test]
    fn padding_byte_on_packet_boundary() {
        assert_eq!(tx_len(60, 64), 60);
        assert_eq!(tx_len(64, 64), 65);
        assert_eq!(tx_len(1024, 512), 1025);
        assert_eq!(tx_len(1514, 512), 1514);
        assert_eq!(tx_len(1536, 512), 1537);
    }

    #[test]
    fn rx_lengths() {
        assert!(!rx_frame_ok(0, 1514));
        assert!(!rx_frame_ok(13, 1514));
        assert!(rx_frame_ok(14, 1514));
        assert!(rx_frame_ok(1514, 1514));
        assert!(rx_frame_ok(1518, 1514));
        assert!(!rx_frame_ok(1519, 1514));
    }
}
