//! UFS host controller driver logic for LucinaOS (UFSHCI, JESD223; UFS, JESD220).
//!
//! The behaviour follows Linux `drivers/ufs/core/ufshcd.c` and `include/ufs/` (the original), with U-Boot's
//! `drivers/ufs/ufs.c` as the smaller reference; the code is our own. The kernel side (`aster-ufs`, patch series
//! 03xx) supplies register access, DMA memory and a clock through the [`Regs`], [`DmaArea`] and [`Clock`] traits and
//! turns [`Host`] into block devices.
//!
//! * [`regs`]: register offsets and bits.
//! * [`upiu`]: UTP transfer request descriptors, PRDT entries, UPIUs (encode, decode).
//! * [`scsi`]: the SCSI commands the driver sends and the data it parses.
//! * [`desc`]: device, geometry and unit descriptors.
//! * [`gpt`]: the partition table, read for the bring-up log.
//! * [`policy`]: which logical unit may be written (only the main user LUN) and which commands may reach a unit.
//! * [`host`]: the controller: probe, reuse of the firmware's link or full bring-up, queries, SCSI commands.
//!
//! One request is in flight at a time, in slot 0, and completion is polled: enough for a block device served by one
//! kernel thread, and the simplest state to take over from the firmware.
//!
//! Safety rule from the owner: nothing but the main user LUN is ever written. [`policy::WritePolicy`] decides that
//! from the unit descriptors and [`Host::scsi`] refuses any command outside the per-unit allowlist before it reaches
//! the controller; query requests that write (descriptors, attributes, flags) are not exposed at all, except setting
//! `fDeviceInit` during a full bring-up, which the JEDEC init sequence requires.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod desc;
mod error;
pub mod gpt;
pub mod host;
pub mod policy;
pub mod regs;
pub mod scsi;
pub mod upiu;
pub mod write_rule;

#[cfg(test)]
mod model;
#[cfg(test)]
mod tests;

pub use error::Error;
pub use host::{
    Clock, DataSegment, DmaArea, Host, LinkState, Options, Probe, Regs, ResetPolicy, Started, Unit,
};
pub use policy::{UnitRole, WritePolicy};
