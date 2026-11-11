// SPDX-License-Identifier: GPL-2.0-only
// Derived from drivers/tty/serial/samsung_tty.c of AsahiLinux/linux (asahi, rev 6ba0637d1ed9; Ben Dooks, Copyright (c)
// 2003-2008 Simtec Electronics). See the crate README.
// This is a Rust port of the behaviour of drivers/tty/serial/samsung_tty.c and include/linux/serial_s3c.h; the original is the source of truth.

//! The one thing the driver talks to: the UART's register window.

/// The UART's MMIO window. Offsets are bytes from the start of the port (DT `reg`, `0x3ad200000` on the MacBook Air M4,
/// t8132.dtsi:442). Every access is one plain 32-bit load or store (see [`crate::regs::REG_WIDTH_BYTES`]) and the
/// implementation must not merge, split or reorder them. The mapping must be device memory, nGnRnE: Linux overrides its
/// early-console fixmap entry with `PROT_DEVICE_nGnRnE` for this UART (samsung_tty.c:2871-2873).
pub trait Mmio {
    fn read32(&self, off: usize) -> u32;
    fn write32(&mut self, off: usize, v: u32);
}
