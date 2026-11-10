//! The e-ink panel of the Kindle Paperwhite 3 (i.MX6 SoloLite): the EPDC controller, the panel's power on the MAX77696,
//! its waveform file and its temperature.
//!
//! * [`waveform`] reads the waveform file's header and maps modes and temperatures onto it.
//! * [`off`] powers the device off and puts back what that resets on the PMIC; it writes nothing else there.
//! * [`pmic`] is the only other way to the PMIC. It refuses every register outside a short list, and values other than
//!   this panel's: a wrong voltage can damage the panel, and other registers there cut the device's power.
//! * [`soc`] has the clocks, the display power domain and the panel's pads.
//! * [`engine`] runs updates: the power sequence, the controller's registers, the choice of a LUT, collisions, completion.
//! * [`convert`] turns the framebuffer's pixels into what the controller reads.
//! * [`touch`] is the touch controller on the panel.
//!
//! The kernel gives the crate register access through [`Hw`] and the PMIC through [`pmic::PmicBus`], so that all of it is
//! tested on the host against a model of the hardware, including the order the panel's rails must keep.
//!
//! Sources of truth for the behaviour: Amazon's Linux 3.0.35 for this board (`CONFIG_MX6SL_WARIO_BASE`) and the mainline
//! based Linux that runs on it now (`drivers/gpu/drm/mxc-epdc`, `max77696-epd.c`, `max77696_thermal.c`). Where they differ
//! this follows what already ran on this device: the Linux and the Phase 0 kernel. No code is copied from them.
//!
//! `no_std` with `alloc`, no `unsafe`, no dependencies.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod battery;
pub mod convert;
pub mod engine;
pub mod i2c;
pub mod off;
pub mod panel;
pub mod pmic;
pub mod soc;
pub mod touch;
pub mod waveform;

/// Register access by physical address, and time. The blocks are in [`soc`] and [`engine`].
pub trait Hw {
    /// Reads a 32-bit register.
    fn read32(&mut self, addr: usize) -> u32;
    /// Writes a 32-bit register.
    fn write32(&mut self, addr: usize, val: u32);
    /// Waits at least this long, without sleeping.
    fn delay_us(&mut self, us: u32);
    /// Makes earlier writes to memory visible to the controller before later register writes.
    fn barrier(&mut self);
}

/// What can go wrong with the panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The I2C bus stayed busy, or a transfer did not finish in time.
    I2cTimeout,
    /// The device on the bus did not acknowledge.
    I2cNak,
    /// A write to a PMIC register that is not on the list, or a value that is not this panel's.
    PmicRefused { reg: u8, val: u8 },
    /// The panel's rails did not come up in time.
    PowerGoodTimeout,
    /// The PMIC reports a fault on a panel rail: the bits of `EPDINTS`.
    PanelFault(u8),
    /// A write-back of a PMIC register did not read back.
    PmicVerify { reg: u8 },
    /// The controller's reset did not finish.
    ControllerReset,
    /// The waveform file is not one this driver can read.
    BadWaveform,
    /// The display power domain did not come up.
    PowerDomain,
    /// The ADC did not finish a conversion.
    AdcTimeout,
    /// The PMIC still reads a rail as on, after it was told to switch it off.
    RailsStillOn,
    /// The touch controller did not answer in time.
    TouchTimeout,
    /// The touch controller's system information makes no sense.
    TouchBadSysinfo,
    /// The touch controller has reset itself, and needs starting again.
    TouchReset,
    /// The panel has had an update in this boot already, so a saved working buffer can no longer be taken.
    PanelStarted,
}

#[cfg(test)]
pub(crate) mod model;
