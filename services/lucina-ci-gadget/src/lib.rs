//! The USB port of the Kindle Paperwhite 3 (i.MX6 SoloLite) as a device: what lets a host reach the running system
//! over the cable, with CDC ECM for the network and CDC ACM next to it, as `g_cdc` does under Linux.
//!
//! * [`soc`] powers the port: I2C1 to the MAX77696 for the PHY's 3.0 V (LDO2), the clocks, the forced VBUS session
//!   (VBUS reaches only the PMIC on this board), the over-current and wake-up settings, and the PHY.
//! * [`dcd`] drives the ChipIdea device controller: queue heads, transfer descriptors, setup packets.
//! * [`cdc`] is the gadget: descriptors, ep0 requests, the ECM notifications and the bulk data path.
//! * [`dhcp`] answers the host's DHCP on the link, which `udhcpd` cannot do here as the kernel has no packet sockets.
//!
//! The kernel gives the crate register and memory access through [`Hw`] and [`DmaMem`], so that all of it is tested on
//! the host against a model of the hardware. It is one thread's job: only [`dcd::Dcd::mask_interrupts`] is for the
//! interrupt handler.
//!
//! Sources of truth for the behaviour: Amazon's Linux 3.0.35 for this board (`CONFIG_MX6SL_WARIO_BASE`) and the mainline
//! based Linux that runs on it now (`drivers/usb/chipidea`, `phy-mxs-usb.c`, `f_ecm.c`, `f_acm.c`). Both are GPL-2.0: read
//! for facts and behaviour only, nothing is copied. Where they differ, this follows the kernel known to work on this
//! device: LDO2 and the session stay on whatever the cable does, and the PHY keeps its default tuning.
//!
//! `no_std` with `alloc`, no `unsafe`, no dependencies.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod cdc;
pub mod dcd;
pub mod dhcp;
pub mod fastboot;
pub mod soc;

/// Register access by physical address, and time. The blocks are in [`soc`] and [`dcd`].
pub trait Hw {
    /// Reads a 32-bit register.
    fn read32(&mut self, addr: usize) -> u32;
    /// Writes a 32-bit register.
    fn write32(&mut self, addr: usize, val: u32);
    /// Reads a 16-bit register.
    fn read16(&mut self, addr: usize) -> u16;
    /// Writes a 16-bit register.
    fn write16(&mut self, addr: usize, val: u16);
    /// Waits at least this long. Used only while bringing up or resetting the port.
    fn delay_us(&mut self, us: u32);
    /// Orders [`DmaMem`] accesses against register accesses, both ways: what was written reaches the controller before
    /// later register writes, and what a register announced is there for later reads.
    fn barrier(&mut self);
}

/// The memory that the controller reads and writes, not cached, [`dcd::DMA_SIZE`] bytes from a page boundary.
pub trait DmaMem {
    /// Its physical address.
    fn paddr(&self) -> u32;
    /// Reads a word at this offset, which is a multiple of 4.
    fn read32(&self, offset: usize) -> u32;
    /// Writes a word at this offset, which is a multiple of 4.
    fn write32(&mut self, offset: usize, val: u32);
    /// Reads bytes at this offset.
    fn read(&self, offset: usize, out: &mut [u8]);
    /// Writes bytes at this offset.
    fn write(&mut self, offset: usize, data: &[u8]);
}

/// What can go wrong while bringing up or resetting the port.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The I2C bus stayed busy, or a transfer did not finish in time.
    I2cTimeout,
    /// The PMIC did not acknowledge.
    I2cNak,
    /// The PLL for the port did not lock.
    PllNotLocked,
    /// The PHY's reset did not finish: its control register still says so.
    PhyReset { ctrl: u32 },
    /// The controller's reset did not finish, which happens without a PHY clock.
    ControllerReset,
    /// The controller would not enter device mode.
    NotDeviceMode,
}

#[cfg(test)]
pub(crate) mod model;
