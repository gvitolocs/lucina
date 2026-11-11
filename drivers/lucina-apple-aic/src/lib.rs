// SPDX-License-Identifier: GPL-2.0-or-later
// Derived from drivers/irqchip/irq-apple-aic.c of AsahiLinux/linux (branch asahi, rev 6ba0637d1ed9):
// Copyright The Asahi Linux Contributors; based on irq-lpc32xx (Copyright 2015-2016 Vladimir Zapolskiy) and
// irq-bcm2836 (Copyright 2015 Broadcom). Behaviour re-expressed in Rust for LucinaOS; see the crate README.
// This is a Rust port of the behaviour of drivers/irqchip/irq-apple-aic.c; the original is the source of truth.

//! The Apple Interrupt Controller of Apple Silicon Macs (AIC v1, v2 and v3; the MacBook Air M4 has a `t8132-aic3`, which
//! the driver takes as `t8122-aic3`), as pure logic over register traits, for LucinaOS's Mac M4 port (Juno on the
//! `macbook-air-m4` board).
//!
//! The behaviour is that of Asahi Linux's `drivers/irqchip/irq-apple-aic.c` at the pinned revision; every constant and rule
//! cites the line it comes from. The crate does no MMIO and no system register access itself: the kernel implements
//! [`AicMmio`], [`AicSysregs`] and [`AicHandler`] over the hardware, and the tests implement them over a software model.
//!
//! - [`info`]: which AIC a device-tree node is, and the register geometry derived from its capability registers;
//! - [`hwirq`]: the packed hwirq number (Verus-checked, `verus/rules.rs`);
//! - [`fwspec`]: device-tree specifiers to hwirqs and back, chip choice and trigger types;
//! - [`chip`]: mask/unmask/eoi/affinity, the FIQ chip, the IRQ and FIQ entry points, IPIs, per-CPU setup, bulk init;
//! - [`ipi_mux`]: the 32 virtual IPIs over one hardware IPI (from `kernel/irq/ipi-mux.c`);
//! - [`regs`]: every register offset, field and constant.
#![no_std]
#![forbid(unsafe_code)]
#![allow(unexpected_cfgs)]

pub mod chip;
pub mod fwspec;
pub mod hwirq;
pub mod info;
pub mod ipi_mux;
pub mod mmio;
pub mod regs;

pub use chip::{AffinityError, Aic, FiqShadow};
pub use fwspec::{ChipKind, Decoded, TranslateError, Translated};
pub use info::{AicInfo, Caps, Geometry, Version};
pub use mmio::{AicHandler, AicMmio, AicSysregs, Report, Sysreg};

#[cfg(any(test, feature = "sim"))]
extern crate std;

/// The software AIC and CPU of the tests, also for the host tests of crates built on this one (feature `sim`; needs `std`,
/// never in the kernel).
#[cfg(any(test, feature = "sim"))]
pub mod sim;
#[cfg(test)]
mod tests;
