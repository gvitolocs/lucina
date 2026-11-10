//! The eMMC of a Kindle Paperwhite 3, over uSDHC2 of the i.MX6SoloLite.
//!
//! Source of truth for behaviour: Amazon's tree for this device (`drivers/mmc/host/sdhci-esdhc-imx.c`,
//! `drivers/mmc/core/mmc.c`, WARIO_BASE) and the Linux that runs on it now (`sdhci-esdhc-imx.c`, `mmc/core/mmc.c`). This is
//! not `lucina-sdhci` with a translation in front: the uSDHC's registers from 0x0c and from 0x28 are not SDHCI's, and
//! neither kernel uses ADMA2 on this chip, because of erratum ERR004536 ("ADMA Length Mismatch Error occurs if the AHB read
//! access is slow, when reading data from the card"). Both run SDMA, so does this.
//!
//! The card holds the only copy of everything on the device, and its boot partitions are what bring it back, so:
//!
//! * [`allowed`] is the only way a command reaches the card, and it knows no command that writes.
//! * [`SWITCHABLE`] lists the bytes of EXT_CSD that may be set: two, both lost at a reset. A card found with another
//!   partition selected is refused, as selecting one means writing a byte that also holds what the card boots from.
//!
//! One request at a time, polled.
//!
//! * [`regs`]: the registers.
//! * [`card`]: what the card reports.
//! * [`host`]: bring-up, commands and reads.

#![no_std]
#![forbid(unsafe_code)]

pub mod card;
mod error;
pub mod host;
pub mod regs;
pub mod soc;

#[cfg(test)]
mod model;
#[cfg(test)]
mod tests;

pub use error::Error;
pub use host::{Clock, Config, DataSegment, DmaArea, Host, Probe, Regs, SWITCHABLE, allowed};
