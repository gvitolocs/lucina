// SPDX-License-Identifier: GPL-2.0-only
//! The Adreno 618 of the Snapdragon 7c Gen 2 (SC7180), without any device model around it: a sequence over register
//! windows and uncached memory that the kernel provides.
//!
//! Linux `drivers/gpu/drm/msm/adreno` (GPL-2.0) is the source of truth for behaviour. Scarlet's
//! `drivers/gpu/qcom-adreno-a618` (petitstrawberry/scarlet-project-chromebook, GPL-2.0-only) was the starting point for
//! the GMU, HFI and PM4 layouts. Its zap-less path for Chromebooks and everything tied to its kernel are not used here.
//! `docs/research/juno-gpu.md` says what ran on the laptop.
//!
//! Every window is addressed by byte offset through [`lucina_qcom_soc::io::RegIo`]. [`Hal`] is the rest of the machine.
#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod a2d;
pub mod clocks;
pub mod command;
pub mod cp;
pub mod fault_log;
pub mod gmu;
pub mod gpu_rules;
pub mod gpuvm;
pub mod hfi;
pub mod opp;
pub mod power;
pub mod pm4;
pub mod quad;
pub mod regs;
pub mod smmu;
pub mod submit;
pub mod uapi;
pub mod scan_rules;
pub mod yuv_rules;

use core::fmt;

/// The rest of the machine: time, barriers and where the driver reports.
pub trait Hal {
    fn now_us(&self) -> u64;
    fn delay_us(&self, us: u32);
    /// `dsb sy`: what was written to uncached memory reaches it before the next device write, and the other way around.
    fn barrier(&self);
    fn log(&self, args: fmt::Arguments);
}

/// What stopped a step, and the register value that shows why.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Error {
    pub what: &'static str,
    pub value: u32,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (value {:#010x})", self.what, self.value)
    }
}

pub type Result<T> = core::result::Result<T, Error>;

pub(crate) fn err<T>(what: &'static str, value: u32) -> Result<T> {
    Err(Error { what, value })
}

/// Polls `read` every `step_us` until `done`, for at most `timeout_us`. Reports the last value read on a timeout.
pub(crate) fn poll<H: Hal>(
    hal: &H,
    what: &'static str,
    timeout_us: u64,
    step_us: u32,
    read: impl Fn() -> u32,
    done: impl Fn(u32) -> bool,
) -> Result<u32> {
    let start = hal.now_us();
    loop {
        let value = read();
        if done(value) {
            return Ok(value);
        }
        if hal.now_us().saturating_sub(start) >= timeout_us {
            return err(what, value);
        }
        hal.delay_us(step_us);
    }
}
