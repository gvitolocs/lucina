// SPDX-License-Identifier: GPL-2.0-only
// Derived from drivers/tty/serial/samsung_tty.c (SPDX GPL-2.0; Ben Dooks, Copyright (c) 2003-2008 Simtec Electronics) and
// include/linux/serial_s3c.h (SPDX GPL-2.0+; Shane Nay, Simtec Electronics) of AsahiLinux/linux (branch asahi, rev
// 6ba0637d1ed9). Behaviour re-expressed in Rust for LucinaOS; see the crate README.
// This is a Rust port of the behaviour of drivers/tty/serial/samsung_tty.c and include/linux/serial_s3c.h; the original is the source of truth.

//! The Apple S5L UART (`apple,s5l-uart`; on the MacBook Air M4 `serial@3ad200000`, t8132.dtsi:442) as polled logic over a
//! register trait, for LucinaOS's early console on the `macbook-air-m4` board.
//!
//! The behaviour is that of Asahi Linux's `drivers/tty/serial/samsung_tty.c` at the pinned revision for `TYPE_APPLE_S5L`;
//! every offset, bit and rule cites the line it comes from. The crate does no MMIO itself: the kernel implements [`Mmio`]
//! over the mapped port, and the tests implement it over a software register file.
//!
//! - [`regs`]: register offsets, bits and masks;
//! - [`mmio`]: the register window trait;
//! - [`uart`]: [`AppleUart`], the transmit and receive tests, send and receive, and the interrupt-bit helpers.
#![no_std]
#![forbid(unsafe_code)]

pub mod mmio;
pub mod regs;
pub mod uart;

pub use mmio::Mmio;
pub use uart::{AppleUart, Pending, TxRule, matches};

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod sim;
#[cfg(test)]
mod tests;
