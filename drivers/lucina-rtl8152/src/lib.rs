//! Realtek RTL8152 USB 2 Fast Ethernet (the Galaxy Book Go's adapter: VID 0bda, PID 8152) for the LucinaOS kernel, in
//! the chip's vendor mode (the configuration with interface class 0xff). The kernel uses it unless given
//! `lucina.rtl8152=off`.
//!
//! * [`UsbTransport`] is all the driver needs from the USB host stack: vendor control transfers, the two bulk
//!   endpoints, a delay and a log line. The kernel's `aster-usb-net` implements it on crab-usb.
//! * [`desc`] finds the vendor configuration and its bulk endpoints in a raw configuration descriptor.
//! * [`Rtl8152`] is the device: [`Rtl8152::bring_up`] (MAC address, chip version, U-Boot's `r8152b_init`,
//!   `rtl8152_up` and autonegotiation), [`Rtl8152::poll_link`], [`Rtl8152::send`] and [`Rtl8152::receive`]. It
//!   implements `lucina_usbnet::EthernetBackend`, through which the kernel's USB network glue moves frames.
//! * [`frame`] is the bulk framing: a descriptor per transmitted frame, several frames per received transfer.
//! * [`regs`] and [`Version`] name the registers and the chip versions. RTL8153 and RTL8153B are detected but
//!   refused with [`Error::UnsupportedChip`]; adding them means implementing their init (U-Boot `r8153_init`,
//!   `r8153_first_init`, and their firmware patches, which Linux treats as required) behind [`Family`].
//!
//! Source of truth for behaviour: U-Boot `drivers/usb/eth/r8152.c` (v2025.10), Linux `drivers/net/usb/r8152.c` (v6.12)
//! and OpenBSD `sys/dev/usb/if_ure.c`, read for behaviour; no code is taken from them.
//! `docs/research/rtl8152.md` section 2 says where they differ and which one this driver follows. The firmware patches
//! of U-Boot's `r8152b_firmware` are left out: Linux and OpenBSD run every RTL8152 without them.
//!
//! `no_std` with `alloc`, no `unsafe`. Tested against a mock chip on the host (QEMU has no RTL8152 model) and on the
//! laptop, where DHCP and SSH run through it, see `docs/research/rtl8152.md` section 4.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod desc;
mod driver;
pub mod frame;
mod ocp;
pub mod regs;
mod transport;
mod usbnet;
mod version;

pub use driver::{Error, Link, MAX_WAIT_MS, QUIET_LOG_MS, RX_BUF_LEN, Rtl8152, Speed, Stats};
pub use transport::{TransportError, UsbTransport, VendorRequest};
pub use version::{Family, Version};

/// MTU of the Ethernet link (frames of up to 1514 bytes without FCS).
pub const MTU: usize = 1500;

/// The IDs this driver takes. U-Boot's table is longer, but its other entries are RTL8153 adapters (their version
/// decides, see [`Version::family`]) and none of them has been run here.
pub const DEVICE_IDS: &[(u16, u16)] = &[(0x0bda, 0x8152)];

/// Whether this driver handles the device with these IDs.
pub fn matches(vendor: u16, product: u16) -> bool {
    DEVICE_IDS.contains(&(vendor, product))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn laptops_adapter_matches() {
        assert!(matches(0x0bda, 0x8152));
        assert!(!matches(0x0bda, 0x8153)); // RTL8153: later
        assert!(!matches(0x8152, 0x0bda));
    }
}
