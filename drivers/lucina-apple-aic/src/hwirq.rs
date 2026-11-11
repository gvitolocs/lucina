// SPDX-License-Identifier: GPL-2.0-or-later
// Derived from drivers/irqchip/irq-apple-aic.c of AsahiLinux/linux (branch asahi, rev 6ba0637d1ed9):
// Copyright The Asahi Linux Contributors; based on irq-lpc32xx (Copyright 2015-2016 Vladimir Zapolskiy) and
// irq-bcm2836 (Copyright 2015 Broadcom). Behaviour re-expressed in Rust for LucinaOS; see the crate README.
// This is a Rust port of the behaviour of drivers/irqchip/irq-apple-aic.c; the original is the source of truth.

//! The packed hwirq number: the layout of the AIC event register, reused as the irq domain's hwirq.
//!
//! - die in bits 31:24 (`AIC_EVENT_DIE`, irq-apple-aic.c:78), type in 23:16 (`AIC_EVENT_TYPE`, :79), number in 15:0
//!   (`AIC_EVENT_NUM`, :80);
//! - a hardware IRQ is `AIC_IRQ_HWIRQ(die, irq)` = die | type IRQ (1) | irq (:210-212, type :83);
//! - a FIQ is `AIC_FIQ_HWIRQ(x)` = type FIQ (0) | x, die 0 (:213-214, type :82);
//! - `AIC_HWIRQ_IRQ` / `AIC_HWIRQ_DIE` take the number and the die back (:215-216).
//!
//! `FIELD_PREP` shifts the value into place and masks it to the field, so out-of-range inputs lose their high bits rather than
//! spilling into the next field; the functions below do the same. Verus checks this file as shipped
//! (`drivers/lucina-apple-aic/verus/rules.rs`; not yet a CI unit, see the README): each field reads back what was packed,
//! and repacking an unpacked hwirq gives the same hwirq.
#![allow(unexpected_cfgs)]

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

#[cfg(verus_keep_ghost)]
verus! {

/// `FIELD_GET(AIC_EVENT_DIE, h)` (irq-apple-aic.c:78, :216).
pub open spec fn die_of(h: u32) -> u32 {
    (h & 0xff00_0000u32) >> 24u32
}

/// `FIELD_GET(AIC_EVENT_TYPE, h)` (irq-apple-aic.c:79).
pub open spec fn type_of(h: u32) -> u32 {
    (h & 0x00ff_0000u32) >> 16u32
}

/// `FIELD_GET(AIC_EVENT_NUM, h)` (irq-apple-aic.c:80, :215).
pub open spec fn num_of(h: u32) -> u32 {
    h & 0xffffu32
}

} // verus!

/// `AIC_HWIRQ_DIE(h)`: bits 31:24 (irq-apple-aic.c:216 with :78).
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == die_of(h),
        r < 256,
))]
pub fn hwirq_die(h: u32) -> u32 {
    #[cfg(verus_keep_ghost)]
    proof! { assert(((h & 0xff00_0000u32) >> 24u32) < 256) by (bit_vector); }
    (h & 0xff00_0000u32) >> 24u32
}

/// `FIELD_GET(AIC_EVENT_TYPE, h)`: bits 23:16 (irq-apple-aic.c:79, used :412 and :637).
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == type_of(h),
        r < 256,
))]
pub fn hwirq_type(h: u32) -> u32 {
    #[cfg(verus_keep_ghost)]
    proof! { assert(((h & 0x00ff_0000u32) >> 16u32) < 256) by (bit_vector); }
    (h & 0x00ff_0000u32) >> 16u32
}

/// `AIC_HWIRQ_IRQ(h)`: bits 15:0 (irq-apple-aic.c:215 with :80).
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == num_of(h),
        r < 65536,
))]
pub fn hwirq_irq(h: u32) -> u32 {
    #[cfg(verus_keep_ghost)]
    proof! { assert((h & 0xffffu32) < 65536) by (bit_vector); }
    h & 0xffffu32
}

/// `AIC_IRQ_HWIRQ(die, irq)` (irq-apple-aic.c:210-212): die in 31:24, type `AIC_EVENT_TYPE_IRQ` = 1 (:83) in 23:16, irq in
/// 15:0, each `FIELD_PREP`-masked to its field.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == (((die << 24u32) & 0xff00_0000u32) | 0x0001_0000u32 | (irq & 0xffffu32)),
        type_of(r) == 1,
        die_of(r) == die % 256,
        num_of(r) == irq % 65536,
))]
pub fn irq_hwirq(die: u32, irq: u32) -> u32 {
    let r = ((die << 24u32) & 0xff00_0000u32) | 0x0001_0000u32 | (irq & 0xffffu32);
    #[cfg(verus_keep_ghost)]
    proof! {
        assert(((r & 0x00ff_0000u32) >> 16u32) == 1
            && ((r & 0xff00_0000u32) >> 24u32) == die % 256
            && (r & 0xffffu32) == irq % 65536) by (bit_vector)
            requires r == (((die << 24u32) & 0xff00_0000u32) | 0x0001_0000u32 | (irq & 0xffffu32));
    }
    r
}

/// `AIC_FIQ_HWIRQ(x)` (irq-apple-aic.c:213-214): type `AIC_EVENT_TYPE_FIQ` = 0 (:82), x in 15:0, die 0.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == (x & 0xffffu32),
        type_of(r) == 0,
        die_of(r) == 0,
        num_of(r) == x % 65536,
))]
pub fn fiq_hwirq(x: u32) -> u32 {
    let r = x & 0xffffu32;
    #[cfg(verus_keep_ghost)]
    proof! {
        assert(((r & 0x00ff_0000u32) >> 16u32) == 0
            && ((r & 0xff00_0000u32) >> 24u32) == 0
            && (r & 0xffffu32) == x % 65536) by (bit_vector)
            requires r == (x & 0xffffu32);
    }
    r
}

/// Unpacks `h` and packs it again with the constructor its type names: [`irq_hwirq`] for type IRQ, [`fiq_hwirq`] for type FIQ
/// on die 0. Every hwirq the driver hands out is of one of these two shapes, and for both the result is `h` itself: nothing
/// is lost between the packed number and its fields. Any other shape is returned as it is.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == h,
))]
pub fn repack(h: u32) -> u32 {
    let ty = hwirq_type(h);
    let die = hwirq_die(h);
    let num = hwirq_irq(h);
    if ty == 1 {
        let r = irq_hwirq(die, num);
        #[cfg(verus_keep_ghost)]
        proof! {
            assert(((h & 0x00ff_0000u32) >> 16u32) == 1 ==>
                ((((((h & 0xff00_0000u32) >> 24u32) << 24u32) & 0xff00_0000u32) | 0x0001_0000u32 | ((h & 0xffffu32) & 0xffffu32)) == h))
                by (bit_vector);
        }
        r
    } else if ty == 0 && die == 0 {
        let r = fiq_hwirq(num);
        #[cfg(verus_keep_ghost)]
        proof! {
            assert((((h & 0x00ff_0000u32) >> 16u32) == 0 && ((h & 0xff00_0000u32) >> 24u32) == 0) ==>
                (((h & 0xffffu32) & 0xffffu32) == h))
                by (bit_vector);
        }
        r
    } else {
        h
    }
}
