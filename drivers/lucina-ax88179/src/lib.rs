//! ASIX AX88179 USB 3 gigabit Ethernet (the owner's adapter: VID 0b95, PID 1790) for the LucinaOS kernel.
//!
//! * [`UsbTransport`] is all the driver needs from the USB host stack: vendor control transfers, the two bulk
//!   endpoints and a delay. The xHCI stack implements it for a device that [`matches`].
//! * [`Ax88179`] is the device: [`Ax88179::probe`] (chip reset, MAC address, LED/EEE setup, autonegotiation),
//!   [`Ax88179::start`] (wait for the link, set the medium mode and bulk-in queue for it), [`Ax88179::poll_link`],
//!   [`Ax88179::send`] and [`Ax88179::receive`]. The network stack's `AnyNetworkDevice` (aster-bigtcp) wraps it.
//! * [`Ax88179`] implements `lucina_usbnet::EthernetBackend`, through which the kernel's USB network glue moves
//!   frames and polls the link ([`Ax88179::poll_link_timed`]).
//! * [`frame`] is the bulk framing: an 8-byte header per transmitted frame, several frames per received transfer.
//!
//! Source of truth for behaviour: Linux 7.1 `drivers/net/usb/ax88179_178a.c` (GPL-2.0+: read for behaviour, not
//! copied). NetBSD `sys/dev/usb/if_axen.c` (BSD) is a cross-check for the RX framing; where this driver follows it
//! instead of Linux, the item says so (`frame` module).
//!
//! `no_std` with `alloc` (the 26 KiB receive buffer), no `unsafe`. Tested against a mock device on the host; the
//! real test is the laptop (QEMU has no AX88179 model), see `docs/research/ax88179.md`.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

mod driver;
pub mod frame;
pub mod regs;
mod transport;
mod usbnet;

pub use driver::{
    Ax88179, Error, LINK_TIMEOUT_MS, Link, POLL_MS, RX_BUF_LEN, Speed, Stats, TX_FIFO_WAIT_MS, UsbSpeed,
};
pub use transport::{TransportError, UsbTransport, VendorRequest};
pub use usbnet::link_text;

/// Configuration the host selects before probing.
pub const CONFIGURATION: u8 = 1;
/// The interface with all three endpoints.
pub const INTERFACE: u8 = 0;
/// Interrupt IN endpoint (link change notifications; unused: the driver polls the PHY).
pub const EP_INTERRUPT_IN: u8 = 0x81;
/// Bulk IN endpoint (received frames).
pub const EP_BULK_IN: u8 = 0x82;
/// Bulk OUT endpoint (frames to send).
pub const EP_BULK_OUT: u8 = 0x03;

/// MTU of the Ethernet link (frames of up to 1514 bytes without FCS).
pub const MTU: usize = 1500;

/// Linux's ID table: the AX88179 and the adapters built on it or on the AX88178A. Only 0b95:1790 has been run.
pub const DEVICE_IDS: &[(u16, u16)] = &[
    (0x0b95, 0x1790), // ASIX AX88179
    (0x0b95, 0x178a), // ASIX AX88178A
    (0x2001, 0x4a00), // D-Link DUB-1312
    (0x0df6, 0x0072), // Sitecom
    (0x04e8, 0xa100), // Samsung
    (0x17ef, 0x304b), // Lenovo
    (0x04b4, 0x3610), // GX3
];

/// Whether this driver handles the device with these IDs.
pub fn matches(vendor: u16, product: u16) -> bool {
    DEVICE_IDS.contains(&(vendor, product))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owners_adapter_matches() {
        assert!(matches(0x0b95, 0x1790));
        assert!(!matches(0x0b95, 0x7720)); // AX88772: a different driver
        assert!(!matches(0x1790, 0x0b95));
    }
}
