//! Finding a CDC Ethernet function in a configuration descriptor.
//!
//! A CDC Ethernet function is two interfaces (CDC 1.2 section 3.4, ECM 1.2 section 3, NCM 1.0 section 5):
//!
//! * a Communications Class interface (class 0x02, subclass 0x06 ECM or 0x0d NCM) carrying class-specific
//!   functional descriptors (Header, Union, Ethernet Networking, for NCM also the NCM descriptor) and usually an
//!   interrupt IN endpoint for notifications;
//! * a Data Class interface (class 0x0a), named by the Union descriptor, whose alternate setting 0 has no
//!   endpoints and whose alternate setting 1 has one bulk IN and one bulk OUT endpoint. Traffic flows only while
//!   setting 1 is selected.
//!
//! As Linux's `usbnet_generic_cdc_bind` (cdc_ether.c) and `usbnet_get_endpoints` (usbnet.c) do, the Union
//! descriptor is required, the Ethernet Networking descriptor is required, and the data interface's endpoints are
//! taken from its first alternate setting that has both a bulk IN and a bulk OUT endpoint. Linux's quirks for
//! devices without a Union descriptor (some Zaurus, Ericsson modems) are not carried over.
//!
//! The descriptors are read from the raw bytes of `GET_DESCRIPTOR(CONFIGURATION)`, so nothing here depends on the
//! host stack's own descriptor types.

use alloc::vec::Vec;

/// Descriptor types (USB 2.0 table 9-5, CDC 1.2 table 12).
const DT_INTERFACE: u8 = 0x04;
const DT_ENDPOINT: u8 = 0x05;
const DT_CS_INTERFACE: u8 = 0x24;

/// Class codes.
pub const CLASS_COMM: u8 = 0x02;
pub const CLASS_CDC_DATA: u8 = 0x0a;
/// Communications Class subclasses (CDC 1.2 table 4).
pub const SUBCLASS_ECM: u8 = 0x06;
pub const SUBCLASS_NCM: u8 = 0x0d;

/// Functional descriptor subtypes (CDC 1.2 table 13).
const CDC_HEADER: u8 = 0x00;
const CDC_UNION: u8 = 0x06;
const CDC_ETHERNET: u8 = 0x0f;
const CDC_NCM: u8 = 0x1a;

/// Endpoint transfer types (bmAttributes bits 1:0).
const XFER_BULK: u8 = 0x02;
const XFER_INT: u8 = 0x03;

/// Which CDC Ethernet protocol the function speaks on its data interface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Protocol {
    /// Ethernet Control Model: one Ethernet frame per bulk transfer.
    Ecm,
    /// Network Control Model: frames batched in NTBs.
    Ncm,
}

/// One endpoint as the driver needs it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Endpoint {
    /// bEndpointAddress, direction bit included.
    pub address: u8,
    /// wMaxPacketSize bits 10:0.
    pub max_packet: u16,
    /// bInterval.
    pub interval: u8,
}

/// The Ethernet Networking functional descriptor (CDC ECM 1.2 table 3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EthernetDesc {
    /// String descriptor index of the MAC address (12 hex digits), see [`crate::ecm::parse_mac`].
    pub mac_string: u8,
    pub statistics: u32,
    /// Largest Ethernet frame the device handles, header included, CRC excluded (normally 1514).
    pub max_segment_size: u16,
    pub mc_filters: u16,
    pub power_filters: u8,
}

/// A CDC Ethernet function found in one configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CdcFunction {
    pub protocol: Protocol,
    /// bConfigurationValue of the configuration it was found in.
    pub configuration: u8,
    /// The Communications Class (control) interface.
    pub control_interface: u8,
    /// The Data Class interface.
    pub data_interface: u8,
    /// The data interface's alternate setting with the bulk pair (1 on conforming devices).
    pub data_alternate: u8,
    pub ethernet: EthernetDesc,
    /// bcdNcmVersion and bmNetworkCapabilities of the NCM functional descriptor (NCM only, optional).
    pub ncm: Option<(u16, u8)>,
    /// The control interface's interrupt IN endpoint, if it has one.
    pub notify: Option<Endpoint>,
    pub bulk_in: Endpoint,
    pub bulk_out: Endpoint,
}

impl CdcFunction {
    /// The MTU the device supports: its maximum segment size less the Ethernet header, as Linux sets
    /// `hard_mtu` from `wMaxSegmentSize` (cdc_ether.c). Devices that report less than a header get 1500.
    pub fn mtu(&self) -> usize {
        let segment = self.ethernet.max_segment_size as usize;
        if segment > crate::ETH_HLEN {
            segment - crate::ETH_HLEN
        } else {
            crate::ETH_DATA_LEN
        }
    }
}

/// Why a configuration does not hold a usable CDC Ethernet function.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DescError {
    /// No Communications Class interface with the ECM or NCM subclass.
    NoFunction,
    /// The control interface has no Union functional descriptor.
    NoUnion,
    /// The control interface has no Ethernet Networking functional descriptor.
    NoEthernet,
    /// The Union names a data interface that has no alternate setting with a bulk IN and a bulk OUT endpoint.
    NoBulkPair,
    /// A descriptor is shorter than its type requires or runs past the end of the buffer.
    Malformed,
}

/// One interface alternate setting with what follows it up to the next interface descriptor.
#[derive(Default)]
struct Alt {
    number: u8,
    alternate: u8,
    class: u8,
    subclass: u8,
    endpoints: Vec<(u8, u8, u16, u8)>, // address, attributes, max packet, interval
    union: Option<(u8, u8)>,           // control, first subordinate
    ethernet: Option<EthernetDesc>,
    ncm: Option<(u16, u8)>,
    has_header: bool,
}

fn le16(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

/// Splits a configuration descriptor into interface alternate settings.
fn alternates(raw: &[u8]) -> Result<Vec<Alt>, DescError> {
    let mut alts: Vec<Alt> = Vec::new();
    let mut at = 0;
    while at < raw.len() {
        if raw.len() - at < 2 {
            return Err(DescError::Malformed);
        }
        let len = raw[at] as usize;
        if len < 2 || at + len > raw.len() {
            return Err(DescError::Malformed);
        }
        let d = &raw[at..at + len];
        match d[1] {
            DT_INTERFACE => {
                if len < 9 {
                    return Err(DescError::Malformed);
                }
                alts.push(Alt {
                    number: d[2],
                    alternate: d[3],
                    class: d[5],
                    subclass: d[6],
                    ..Alt::default()
                });
            }
            DT_ENDPOINT => {
                if len < 7 {
                    return Err(DescError::Malformed);
                }
                if let Some(alt) = alts.last_mut() {
                    alt.endpoints.push((d[2], d[3], le16(d, 4) & 0x7ff, d[6]));
                }
            }
            DT_CS_INTERFACE if len >= 3 => {
                let Some(alt) = alts.last_mut() else {
                    at += len;
                    continue;
                };
                match d[2] {
                    CDC_HEADER => alt.has_header = true,
                    // Only the first subordinate interface is the data interface (ECM 1.2 section 5.2).
                    CDC_UNION if len >= 5 => alt.union = Some((d[3], d[4])),
                    CDC_ETHERNET if len >= 13 => {
                        alt.ethernet = Some(EthernetDesc {
                            mac_string: d[3],
                            statistics: u32::from_le_bytes([d[4], d[5], d[6], d[7]]),
                            max_segment_size: le16(d, 8),
                            mc_filters: le16(d, 10),
                            power_filters: d[12],
                        })
                    }
                    CDC_NCM if len >= 6 => alt.ncm = Some((le16(d, 3), d[5])),
                    CDC_UNION | CDC_ETHERNET | CDC_NCM => return Err(DescError::Malformed),
                    _ => {}
                }
            }
            _ => {}
        }
        at += len;
    }
    Ok(alts)
}

fn endpoint(e: &(u8, u8, u16, u8)) -> Endpoint {
    Endpoint {
        address: e.0,
        max_packet: e.2,
        interval: e.3,
    }
}

/// Finds the first CDC Ethernet function (ECM or NCM) in the raw configuration descriptor `raw`, which belongs to
/// the configuration with value `configuration`.
pub fn find_function(raw: &[u8], configuration: u8) -> Result<CdcFunction, DescError> {
    let alts = alternates(raw)?;
    let mut first_error = DescError::NoFunction;
    for control in alts.iter().filter(|a| a.class == CLASS_COMM) {
        let protocol = match control.subclass {
            SUBCLASS_ECM => Protocol::Ecm,
            SUBCLASS_NCM => Protocol::Ncm,
            _ => continue,
        };
        match function_at(&alts, control, protocol, configuration) {
            Ok(function) => return Ok(function),
            Err(error) => {
                if first_error == DescError::NoFunction {
                    first_error = error;
                }
            }
        }
    }
    Err(first_error)
}

fn function_at(
    alts: &[Alt],
    control: &Alt,
    protocol: Protocol,
    configuration: u8,
) -> Result<CdcFunction, DescError> {
    let (master, data_interface) = control.union.ok_or(DescError::NoUnion)?;
    // Linux checks that the Union's master is the interface it sits on ("bogus CDC Union").
    if master != control.number {
        return Err(DescError::NoUnion);
    }
    let ethernet = control.ethernet.ok_or(DescError::NoEthernet)?;
    let notify = control
        .endpoints
        .iter()
        .find(|e| e.1 & 0x03 == XFER_INT && e.0 & 0x80 != 0)
        .map(endpoint);
    let (data_alternate, bulk_in, bulk_out) = alts
        .iter()
        .filter(|a| a.number == data_interface)
        .find_map(|a| {
            let bulk = |dir_in: bool| {
                a.endpoints
                    .iter()
                    .find(|e| e.1 & 0x03 == XFER_BULK && (e.0 & 0x80 != 0) == dir_in)
                    .map(endpoint)
            };
            Some((a.alternate, bulk(true)?, bulk(false)?))
        })
        .ok_or(DescError::NoBulkPair)?;
    Ok(CdcFunction {
        protocol,
        configuration,
        control_interface: control.number,
        data_interface,
        data_alternate,
        ethernet,
        ncm: control.ncm,
        notify,
        bulk_in,
        bulk_out,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// QEMU's usb-net CDC configuration (hw/usb/dev-network.c, `desc_iface_cdc`), as the device sends it:
    /// configuration value 1, control interface 0 (ECM, interrupt IN 0x81, 16 bytes, interval 32), data
    /// interface 1 with alternate setting 0 empty and 1 holding bulk IN 0x82 and bulk OUT 0x02 of 64 bytes.
    pub(crate) const QEMU_ECM: &[u8] = &[
        9, 0x02, 80, 0, 2, 1, 7, 0xc0, 0x32, // configuration
        9, 0x04, 0, 0, 1, 0x02, 0x06, 0x00, 0, // interface 0: comm, ECM
        5, 0x24, 0x00, 0x10, 0x01, // header, CDC 1.10
        5, 0x24, 0x06, 0, 1, // union: master 0, slave 1
        13, 0x24, 0x0f, 15, 0, 0, 0, 0, 0xea, 0x05, 0, 0,
        0, // ethernet: MAC string 15, segment 1514
        7, 0x05, 0x81, 0x03, 16, 0, 0x20, // interrupt IN
        9, 0x04, 1, 0, 0, 0x0a, 0x00, 0x00, 0, // interface 1 alt 0: data, no endpoints
        9, 0x04, 1, 1, 2, 0x0a, 0x00, 0x00, 0, // interface 1 alt 1
        7, 0x05, 0x82, 0x02, 64, 0, 0, // bulk IN
        7, 0x05, 0x02, 0x02, 64, 0, 0, // bulk OUT
    ];

    /// QEMU's usb-net RNDIS configuration: comm subclass ACM, vendor protocol. Not a CDC Ethernet function.
    const QEMU_RNDIS: &[u8] = &[
        9, 0x02, 67, 0, 2, 2, 5, 0xc0, 0x32, //
        9, 0x04, 0, 0, 1, 0x02, 0x02, 0xff, 0, // comm, ACM, vendor protocol
        5, 0x24, 0x00, 0x10, 0x01, //
        5, 0x24, 0x01, 0x00, 1, // call management
        4, 0x24, 0x02, 0x00, // ACM
        5, 0x24, 0x06, 0, 1, // union
        7, 0x05, 0x81, 0x03, 8, 0, 1, //
        9, 0x04, 1, 0, 2, 0x0a, 0x00, 0x00, 0, //
        7, 0x05, 0x82, 0x02, 64, 0, 0, //
        7, 0x05, 0x02, 0x02, 64, 0, 0, //
    ];

    /// A Realtek RTL8152 (0bda:8152) as far as the Galaxy Book Go's log shows it and `lsusb` output of the chip gives
    /// the rest. Configuration 1: vendor-specific interface 0 with bulk 0x81 and 0x02 and interrupt 0x83.
    const RTL8152_VENDOR: &[u8] = &[
        9, 0x02, 39, 0, 1, 1, 0, 0xa0, 50, // configuration 1
        9, 0x04, 0, 0, 3, 0xff, 0xff, 0x00, 0, // interface 0: vendor
        7, 0x05, 0x81, 0x02, 0, 2, 0, // bulk IN
        7, 0x05, 0x02, 0x02, 0, 2, 0, // bulk OUT
        7, 0x05, 0x83, 0x03, 2, 0, 8, // interrupt IN
    ];

    /// Its configuration 2: CDC-ECM, with the same endpoint addresses on other interfaces. That is what crab-usb tripped
    /// over: it looked up interface 0 in configuration 1 (see third_party/crab-usb/LUCINA-CHANGES.md).
    const RTL8152_ECM: &[u8] = &[
        9, 0x02, 80, 0, 2, 2, 0, 0xa0, 50, // configuration 2
        9, 0x04, 0, 0, 1, 0x02, 0x06, 0x00, 5, // interface 0: comm, ECM
        5, 0x24, 0x00, 0x10, 0x01, // header
        5, 0x24, 0x06, 0, 1, // union: master 0, slave 1
        13, 0x24, 0x0f, 3, 0, 0, 0, 0, 0xea, 0x05, 0, 0,
        0, // ethernet: MAC string 3, segment 1514
        7, 0x05, 0x83, 0x03, 16, 0, 8, // interrupt IN
        9, 0x04, 1, 0, 0, 0x0a, 0x00, 0x00, 0, // interface 1 alt 0: data, no endpoints
        9, 0x04, 1, 1, 2, 0x0a, 0x00, 0x00, 0, // interface 1 alt 1
        7, 0x05, 0x81, 0x02, 0, 2, 0, // bulk IN
        7, 0x05, 0x02, 0x02, 0, 2, 0, // bulk OUT
    ];

    #[test]
    fn rtl8152_vendor_configuration_is_passed_over() {
        assert_eq!(find_function(RTL8152_VENDOR, 1), Err(DescError::NoFunction));
    }

    #[test]
    fn rtl8152_ecm_configuration() {
        let f = find_function(RTL8152_ECM, 2).unwrap();
        assert_eq!(f.protocol, Protocol::Ecm);
        assert_eq!(f.configuration, 2);
        assert_eq!(
            (f.control_interface, f.data_interface, f.data_alternate),
            (0, 1, 1)
        );
        assert_eq!(f.ethernet.mac_string, 3);
        assert_eq!(f.mtu(), 1500);
        assert_eq!(
            f.notify.map(|ep| (ep.address, ep.max_packet)),
            Some((0x83, 16))
        );
        assert_eq!((f.bulk_in.address, f.bulk_in.max_packet), (0x81, 512));
        assert_eq!((f.bulk_out.address, f.bulk_out.max_packet), (0x02, 512));
    }

    #[test]
    fn qemu_ecm_configuration() {
        let f = find_function(QEMU_ECM, 1).unwrap();
        assert_eq!(f.protocol, Protocol::Ecm);
        assert_eq!(f.configuration, 1);
        assert_eq!(
            (f.control_interface, f.data_interface, f.data_alternate),
            (0, 1, 1)
        );
        assert_eq!(f.ethernet.mac_string, 15);
        assert_eq!(f.ethernet.max_segment_size, 1514);
        assert_eq!(f.mtu(), 1500);
        assert_eq!(
            f.notify,
            Some(Endpoint {
                address: 0x81,
                max_packet: 16,
                interval: 32
            })
        );
        assert_eq!(f.bulk_in.address, 0x82);
        assert_eq!(f.bulk_out.address, 0x02);
        assert_eq!(f.bulk_out.max_packet, 64);
        assert_eq!(f.ncm, None);
    }

    #[test]
    fn rndis_is_not_cdc_ethernet() {
        assert_eq!(find_function(QEMU_RNDIS, 2), Err(DescError::NoFunction));
    }

    #[test]
    fn ncm_function_with_superspeed_companion() {
        // A typical NCM gadget (Linux f_ncm): IAD, comm NCM with the NCM descriptor, and SuperSpeed endpoint
        // companions (type 0x30) between the endpoints, which must be skipped.
        let raw: &[u8] = &[
            9, 0x02, 0, 0, 2, 3, 0, 0x80, 0x32, //
            8, 0x0b, 0, 2, 0x02, 0x0d, 0x00, 0, // IAD
            9, 0x04, 0, 0, 1, 0x02, 0x0d, 0x00, 4, //
            5, 0x24, 0x00, 0x10, 0x01, //
            5, 0x24, 0x06, 0, 1, //
            13, 0x24, 0x0f, 5, 0, 0, 0, 0, 0xea, 0x05, 0, 0, 0, //
            6, 0x24, 0x1a, 0x00, 0x01, 0x00, // NCM 1.0, no capabilities
            7, 0x05, 0x83, 0x03, 16, 0, 9, //
            6, 0x30, 0, 0, 16, 0, // SS companion
            9, 0x04, 1, 0, 0, 0x0a, 0x00, 0x01, 0, //
            9, 0x04, 1, 1, 2, 0x0a, 0x00, 0x01, 0, //
            7, 0x05, 0x81, 0x02, 0x00, 0x04, 0, //
            6, 0x30, 0, 0, 0, 0, //
            7, 0x05, 0x02, 0x02, 0x00, 0x04, 0, //
            6, 0x30, 0, 0, 0, 0, //
        ];
        let f = find_function(raw, 3).unwrap();
        assert_eq!(f.protocol, Protocol::Ncm);
        assert_eq!(f.ncm, Some((0x0100, 0)));
        assert_eq!(f.bulk_in.max_packet, 1024);
        assert_eq!(f.notify.unwrap().address, 0x83);
    }

    #[test]
    fn union_and_ethernet_are_required() {
        let mut no_union = QEMU_ECM.to_vec();
        no_union[25] = 0x07; // union subtype -> something else
        assert_eq!(find_function(&no_union, 1), Err(DescError::NoUnion));
        let mut no_ether = QEMU_ECM.to_vec();
        no_ether[30] = 0x07;
        assert_eq!(find_function(&no_ether, 1), Err(DescError::NoEthernet));
        let mut bogus_master = QEMU_ECM.to_vec();
        bogus_master[26] = 3;
        assert_eq!(find_function(&bogus_master, 1), Err(DescError::NoUnion));
    }

    #[test]
    fn data_interface_without_bulk_pair() {
        let truncated = &QEMU_ECM[..QEMU_ECM.len() - 7];
        assert_eq!(find_function(truncated, 1), Err(DescError::NoBulkPair));
    }

    #[test]
    fn malformed_lengths() {
        assert_eq!(find_function(&[9, 0x02, 0], 1), Err(DescError::Malformed));
        assert_eq!(find_function(&[0, 0x04], 1), Err(DescError::Malformed));
        let mut short_ether = QEMU_ECM.to_vec();
        short_ether[28] = 5; // ethernet descriptor claims 5 bytes
        assert!(find_function(&short_ether, 1).is_err());
    }
}
