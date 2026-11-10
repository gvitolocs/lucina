//! Finding the vendor mode in a configuration descriptor.
//!
//! An RTL8152 offers its vendor mode and CDC-ECM as different configurations, with the vendor one not always first
//! (Linux `rtl8152_cfgselector_choose_configuration` looks for it). The vendor mode is the configuration whose first
//! interface has class 0xff, carrying a bulk IN, a bulk OUT and an interrupt endpoint. The addresses are taken from
//! the descriptor, as U-Boot does (Linux assumes 0x81, 0x02 and 0x83, which is what the laptop's adapter has).

/// Descriptor types (USB 2.0 table 9-5).
const DT_INTERFACE: u8 = 0x04;
const DT_ENDPOINT: u8 = 0x05;

const CLASS_VENDOR: u8 = 0xff;
const XFER_MASK: u8 = 0x03;
const XFER_BULK: u8 = 0x02;

/// The vendor mode of a chip, as far as the driver needs it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VendorFunction {
    /// `bConfigurationValue`.
    pub configuration: u8,
    pub interface: u8,
    pub alternate: u8,
    /// bEndpointAddress of the bulk IN endpoint.
    pub bulk_in: u8,
    /// bEndpointAddress of the bulk OUT endpoint.
    pub bulk_out: u8,
    /// wMaxPacketSize of the bulk OUT endpoint.
    pub max_packet: u16,
}

/// The vendor mode in the raw bytes of `GET_DESCRIPTOR(CONFIGURATION)` for configuration `configuration`, or `None`
/// when this configuration is not one (or its bytes are broken).
pub fn find_vendor_function(raw: &[u8], configuration: u8) -> Option<VendorFunction> {
    let mut rest = raw;
    let mut found: Option<(u8, u8)> = None;
    let mut bulk_in = None;
    let mut bulk_out = None;
    let mut seen_interface = false;
    while !rest.is_empty() {
        let len = *rest.first()? as usize;
        if len < 2 || len > rest.len() {
            return None;
        }
        let (descriptor, tail) = rest.split_at(len);
        rest = tail;
        match descriptor[1] {
            DT_INTERFACE => {
                if descriptor.len() < 9 {
                    return None;
                }
                if !seen_interface {
                    seen_interface = true;
                    if descriptor[5] != CLASS_VENDOR {
                        return None;
                    }
                    found = Some((descriptor[2], descriptor[3]));
                } else {
                    // Another interface or alternate setting: the endpoints of the first are complete.
                    break;
                }
            }
            DT_ENDPOINT if found.is_some() => {
                if descriptor.len() < 7 || descriptor[3] & XFER_MASK != XFER_BULK {
                    continue;
                }
                let address = descriptor[2];
                let max_packet = u16::from_le_bytes([descriptor[4], descriptor[5]]) & 0x07ff;
                if address & 0x80 != 0 {
                    bulk_in = bulk_in.or(Some(address));
                } else {
                    bulk_out = bulk_out.or(Some((address, max_packet)));
                }
            }
            _ => {}
        }
    }
    let (interface, alternate) = found?;
    let (bulk_out, max_packet) = bulk_out?;
    Some(VendorFunction {
        configuration,
        interface,
        alternate,
        bulk_in: bulk_in?,
        bulk_out,
        max_packet,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    fn endpoint(address: u8, attributes: u8, max_packet: u16) -> Vec<u8> {
        let mp = max_packet.to_le_bytes();
        vec![7, DT_ENDPOINT, address, attributes, mp[0], mp[1], 8]
    }

    fn interface(number: u8, alt: u8, endpoints: u8, class: (u8, u8, u8)) -> Vec<u8> {
        vec![
            9,
            DT_INTERFACE,
            number,
            alt,
            endpoints,
            class.0,
            class.1,
            class.2,
            0,
        ]
    }

    fn configuration(value: u8, body: Vec<Vec<u8>>) -> Vec<u8> {
        let body: Vec<u8> = body.concat();
        let total = (9 + body.len()) as u16;
        let t = total.to_le_bytes();
        let mut out = vec![9, 2, t[0], t[1], 1, value, 0, 0xa0, 50];
        out.extend(body);
        out
    }

    /// Configuration 1 of the Galaxy Book Go's adapter: the interface tuple from the laptop's log, (0, 255, 255, 0).
    pub(crate) fn laptop_vendor_config() -> Vec<u8> {
        configuration(
            1,
            vec![
                interface(0, 0, 3, (0xff, 0xff, 0)),
                endpoint(0x81, 0x02, 512),
                endpoint(0x02, 0x02, 512),
                endpoint(0x83, 0x03, 2),
            ],
        )
    }

    /// Configuration 2: the CDC-ECM of the laptop's log, (0, 2, 6, 0) and (1, 10, ...).
    fn laptop_ecm_config() -> Vec<u8> {
        configuration(
            2,
            vec![
                interface(0, 0, 1, (0x02, 0x06, 0)),
                vec![5, 0x24, 0, 0x10, 0x01],
                vec![5, 0x24, 6, 0, 1],
                vec![13, 0x24, 0x0f, 3, 0, 0, 0, 0, 0xea, 5, 0, 0, 0],
                endpoint(0x83, 0x03, 16),
                interface(1, 0, 0, (0x0a, 0, 0)),
                interface(1, 1, 2, (0x0a, 0, 0)),
                endpoint(0x81, 0x02, 512),
                endpoint(0x02, 0x02, 512),
            ],
        )
    }

    #[test]
    fn finds_the_laptops_vendor_mode() {
        assert_eq!(
            find_vendor_function(&laptop_vendor_config(), 1),
            Some(VendorFunction {
                configuration: 1,
                interface: 0,
                alternate: 0,
                bulk_in: 0x81,
                bulk_out: 0x02,
                max_packet: 512,
            })
        );
    }

    #[test]
    fn the_ecm_configuration_is_not_it() {
        assert_eq!(find_vendor_function(&laptop_ecm_config(), 2), None);
    }

    #[test]
    fn full_speed_packet_size_is_read_from_the_endpoint() {
        let raw = configuration(
            3,
            vec![
                interface(0, 0, 2, (0xff, 0xff, 0)),
                endpoint(0x02, 0x02, 64),
                endpoint(0x81, 0x02, 64),
            ],
        );
        let function = find_vendor_function(&raw, 3).unwrap();
        assert_eq!(function.configuration, 3);
        assert_eq!(function.max_packet, 64);
    }

    #[test]
    fn without_a_bulk_pair_it_is_not_usable() {
        let no_out = configuration(
            1,
            vec![
                interface(0, 0, 2, (0xff, 0xff, 0)),
                endpoint(0x81, 0x02, 512),
                endpoint(0x83, 0x03, 2),
            ],
        );
        assert_eq!(find_vendor_function(&no_out, 1), None);
        let no_in = configuration(
            1,
            vec![
                interface(0, 0, 1, (0xff, 0xff, 0)),
                endpoint(0x02, 0x02, 512),
            ],
        );
        assert_eq!(find_vendor_function(&no_in, 1), None);
    }

    #[test]
    fn endpoints_of_a_later_interface_do_not_count() {
        let raw = configuration(
            1,
            vec![
                interface(0, 0, 1, (0xff, 0xff, 0)),
                endpoint(0x81, 0x02, 512),
                interface(1, 0, 1, (0xff, 0xff, 0)),
                endpoint(0x02, 0x02, 512),
            ],
        );
        assert_eq!(find_vendor_function(&raw, 1), None);
    }

    #[test]
    fn broken_bytes_are_refused() {
        let good = laptop_vendor_config();
        // Every truncation is either refused or still a whole function, and never panics.
        for cut in 0..good.len() {
            let _ = find_vendor_function(&good[..cut], 1);
        }
        assert_eq!(find_vendor_function(&good[..12], 1), None);
        assert_eq!(find_vendor_function(&[], 1), None);
        // A length of zero or one would not advance.
        assert_eq!(find_vendor_function(&[0, 4], 1), None);
        assert_eq!(find_vendor_function(&[1, 4], 1), None);
        // Too short for what its type says.
        assert_eq!(find_vendor_function(&[4, DT_INTERFACE, 0, 0], 1), None);
    }

    #[test]
    fn arbitrary_bytes_never_panic() {
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..20_000 {
            let len = (next() % 80) as usize;
            let raw: Vec<u8> = (0..len)
                .map(|i| {
                    let v = next() as u8;
                    // Mostly plausible lengths and types, so that the walk gets somewhere.
                    if i % 7 == 0 {
                        (v % 10) + 2
                    } else if i % 7 == 1 {
                        4 + (v & 1)
                    } else {
                        v
                    }
                })
                .collect();
            let _ = find_vendor_function(&raw, 1);
        }
    }
}
