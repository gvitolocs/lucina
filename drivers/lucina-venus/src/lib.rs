// SPDX-License-Identifier: GPL-2.0-only
//! The video block of the Snapdragon 7c Gen 2 (SC7180): Venus 4xx (VPU AR50) with its firmware started by the secure world (PAS 9).
//! A sequence over register windows and uncached memory that the kernel provides, without any device model around it.
//!
//! Linux 7.1 `drivers/media/platform/qcom/venus` and `drivers/clk/qcom/videocc-sc7180.c` (GPL-2.0) are the source of truth for
//! behaviour, gone through in the order they run: `docs/research/juno-venus.md` has that list, what is the same, what differs and
//! why. `oracle/` compiles Linux's own HFI files and `tests/oracle.rs` compares their packets with these.
//!
//! [`rules`] holds what must hold whatever the firmware or a file does, proved with Verus.
#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod clocks;
pub mod cpu;
pub mod decode;
pub mod hfi;
pub mod iface;
pub mod power;
pub mod regs;
pub mod rules;
pub mod session;

use core::fmt;

/// The rest of the machine: time and where the driver reports.
pub trait Hal {
    fn now_us(&self) -> u64;
    fn delay_us(&self, us: u32);
    fn log(&self, args: fmt::Arguments);
}

/// What stopped a step, and the register or message value that shows why.
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
