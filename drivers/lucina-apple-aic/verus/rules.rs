// SPDX-License-Identifier: GPL-2.0-or-later
// Derived from drivers/irqchip/irq-apple-aic.c of AsahiLinux/linux (branch asahi, rev 6ba0637d1ed9):
// Copyright The Asahi Linux Contributors; based on irq-lpc32xx (Copyright 2015-2016 Vladimir Zapolskiy) and
// irq-bcm2836 (Copyright 2015 Broadcom). Behaviour re-expressed in Rust for LucinaOS; see the crate README.
// This is a Rust port of the behaviour of drivers/irqchip/irq-apple-aic.c; the original is the source of truth.
// Verus proof of the SHIPPED code, not a model (project rule D-011): drivers/lucina-apple-aic/src/hwirq.rs, the packed hwirq
// of the Apple AIC (irq-apple-aic.c:210-216). Proved in its ensures: each field reads back what was packed (masked to its
// width, as FIELD_PREP), and unpacking then repacking any hwirq gives it back. What follows adds the round trip from the
// fields' side for the ranges the driver uses (die < 256, number < 65536).
#![feature(proc_macro_hygiene)]
#[path = "../src/hwirq.rs"]
pub mod hwirq;

use hwirq::*;
use vstd::prelude::*;

verus! {

/// A hardware IRQ: pack, then unpack, gives the die and the number back.
fn irq_fields_round_trip(die: u32, irq: u32) -> (r: (u32, u32))
    requires
        die < 256,
        irq < 65536,
    ensures
        r == (die, irq),
{
    let h = irq_hwirq(die, irq);
    assert(type_of(h) == 1);
    (hwirq_die(h), hwirq_irq(h))
}

/// A FIQ: pack, then unpack, gives the index back, on die 0.
fn fiq_fields_round_trip(x: u32) -> (r: (u32, u32))
    requires
        x < 65536,
    ensures
        r == (0u32, x),
{
    let h = fiq_hwirq(x);
    (hwirq_die(h), hwirq_irq(h))
}

/// An IRQ hwirq and a FIQ hwirq never coincide: their type fields differ.
fn irq_and_fiq_differ(die: u32, irq: u32, x: u32) -> (r: (u32, u32))
    ensures
        r.0 != r.1,
{
    let a = irq_hwirq(die, irq);
    let b = fiq_hwirq(x);
    assert(type_of(a) != type_of(b));
    (a, b)
}

} // verus!

fn main() {}
