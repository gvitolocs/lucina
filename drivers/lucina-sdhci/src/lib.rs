//! SD cards over an SDHCI host controller, for LucinaOS Juno (Samsung Galaxy Book Go, Qualcomm SC7180).
//!
//! Source of truth for behaviour: Linux `drivers/mmc/host/sdhci.c`, `sdhci-msm.c` and `drivers/mmc/core/sd.c`. The
//! working proof on this SoC, and the source of the SC7180 facts in [`msm`] and [`soc`], is Scarlet's
//! `drivers/mmc/qcom-sdhci-sc7180` (petitstrawberry/scarlet-project-chromebook, GPL-2.0-only, Copyright its authors) with
//! its generic host `kernel/src/drivers/mmc` (petitstrawberry/Scarlet); `rcore-os/tgoskits`
//! `drivers/blk/{sdhci-host,sdmmc-protocol}` (Apache-2.0) was a further reference. The code here is written for this
//! crate; see `docs/research/gbgo-sd.md` and `docs/research/integration-decisions.md`.
//!
//! The kernel side (`aster-sd`, patch 0660) supplies register access, DMA memory and a clock through [`Regs`],
//! [`DmaArea`] and [`Clock`] and turns [`Host`] into a block device.
//!
//! * [`regs`]: SDHCI registers and bits.
//! * [`card`]: what the card reports: OCR, CID, CSD, SCR, switch status.
//! * [`msm`]: Qualcomm SDCC v5 registers and sequences (power requests, vendor registers).
//! * [`soc`]: the SC7180's clocks and pins for SDCC2, the SD slot.
//! * [`host`]: bring-up, commands, ADMA2 and block reads and writes.
//!
//! One request at a time, polled. Default Speed and High Speed at 3 V; UHS-I is left out. Writes need
//! [`Config::writable`]: a host started without it never sends a write command.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod card;
mod error;
pub mod host;
pub mod msm;
pub mod regs;
pub mod soc;

#[cfg(test)]
mod model;
#[cfg(test)]
mod tests;

pub use error::Error;
pub use host::{Clock, Config, DataSegment, DmaArea, Host, Probe, Regs, Variant, quiesce};
