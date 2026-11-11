// SPDX-License-Identifier: GPL-2.0-only
// Derived from arch/arm64/kernel/smp_spin_table.c (SPDX GPL-2.0-only; Copyright (C) 2013 ARM Ltd.) and the simple-framebuffer
// binding (SPDX GPL-2.0) of AsahiLinux/linux (branch asahi, rev 6ba0637d1ed9), and from the behaviour of m1n1 src/kboot.c and
// src/smp.c (rev cf37927d6bf4; GPL-2.0-or-later from v2.0.0, MIT before; no code copied). See the crate README.
// This is a Rust port of the behaviour of arch/arm64/kernel/smp_spin_table.c and the simple-framebuffer binding; the original is the source of truth.

//! What LucinaOS's Mac M4 board (Juno on `macbook-air-m4`, root compatible `apple,j713`) takes from the device tree that m1n1
//! hands over, as pure logic for the kernel's boot code to call and for host tests to check:
//!
//! - [`simplefb`]: the `simple-framebuffer` node that m1n1 fills in under `/chosen` (formats it can write, geometry rules,
//!   `apple,simple-framebuffer` before `simple-framebuffer`);
//! - [`spin_table`]: which way to start a CPU (`psci` or `spin-table`, per `/cpus` node) and the release sequence for a
//!   spin-table CPU, over a trait.
//!
//! The behaviour is read from m1n1 (`src/kboot.c`, `src/smp.c`, rev cf37927d6bf4, under the owner's decision of 2026-11-11
//! that models may read it as a reference; nothing is copied) and Asahi Linux's device trees (rev 6ba0637d1ed9); every rule
//! cites the line it comes from. Nothing here touches hardware.
#![no_std]
#![forbid(unsafe_code)]

pub mod simplefb;
pub mod spin_table;

#[cfg(test)]
extern crate std;
