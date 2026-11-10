// SPDX-License-Identifier: MPL-2.0

//! The only PMIC registers [`crate::irq::write`] may write: the interrupt registers of the power key's PON block and of the RTC
//! alarm, and the alarm's own registers. Nothing else, on any SID: no regulator, charger, reset or thermal register.
//!
//! The registers: `spmi-pmic-arb.c:600-607` (the qpnpint block every peripheral has: SET_TYPE 0x11, POLARITY_HIGH 0x12,
//! POLARITY_LOW 0x13, LATCHED_CLR 0x14, EN_SET 0x15, EN_CLR 0x16) and `rtc-pm8xxx.c:535-543` (`pm8941_regs`: ALARM_RW 0x40..0x43,
//! ALARM_CTRL 0x46, ALARM_CTRL2 0x48). The peripherals: PON at 0x800 on SID 0 (the laptop's `pon@800`, `pwrkey`
//! `interrupts = <0 8 0 3>`) and the alarm at 0x6100 on SID 0 (`pm6150.dtsi` `rtc@6000`, `reg = <0x6000>, <0x6100>`).
//!
//! Verus proves this very file (`verification/juno-power/rules.rs`): what `allowed` says yes to is in its `ensures`.
#![allow(unexpected_cfgs)]

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

/// The power key's PON block: SID 0, peripheral 0x08.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const PON_PPID: u16 = 0x008;
/// The RTC alarm: SID 0, peripheral 0x61.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const ALARM_PPID: u16 = 0x061;

/// Whether `reg` (the low byte of an address) of peripheral `ppid` may be written.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == reg_allowed_spec(ppid, reg),
))]
pub fn reg_allowed(ppid: u16, reg: u16) -> bool {
    let interrupt = 0x11 <= reg && reg <= 0x16;
    let alarm = (0x40 <= reg && reg <= 0x43) || reg == 0x46 || reg == 0x48;
    (ppid == PON_PPID && interrupt) || (ppid == ALARM_PPID && (interrupt || alarm))
}

/// Whether a write of `len` bytes at `addr` (peripheral << 8 | register) to peripheral `ppid` is allowed: 1 to 4 bytes, all in that
/// peripheral, every one an allowed register.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == allowed_spec(ppid, addr, len),
))]
pub fn allowed(ppid: u16, addr: u16, len: usize) -> bool {
    if len == 0 || len > 4 || addr >> 8 != ppid & 0xff {
        return false;
    }
    let first = (addr & 0xff) as u32;
    let len = len as u32;
    if first + len > 0x100 {
        return false;
    }
    reg_allowed(ppid, first as u16)
        && (len < 2 || reg_allowed(ppid, (first + 1) as u16))
        && (len < 3 || reg_allowed(ppid, (first + 2) as u16))
        && (len < 4 || reg_allowed(ppid, (first + 3) as u16))
}

#[cfg(verus_keep_ghost)]
verus! {

pub open spec fn reg_allowed_spec(ppid: u16, reg: u16) -> bool {
    let interrupt = 0x11 <= reg && reg <= 0x16;
    let alarm = (0x40 <= reg && reg <= 0x43) || reg == 0x46 || reg == 0x48;
    (ppid == 0x008 && interrupt) || (ppid == 0x061 && (interrupt || alarm))
}

/// Every byte written is an allowed register of that one peripheral (the unrolled form; `rules.rs` proves the `forall` from it).
pub open spec fn allowed_spec(ppid: u16, addr: u16, len: usize) -> bool {
    let first = (addr & 0xff) as int;
    &&& 1 <= len <= 4
    &&& (addr >> 8) == (ppid & 0xff)
    &&& first + len <= 0x100
    &&& reg_allowed_spec(ppid, first as u16)
    &&& (len < 2 || reg_allowed_spec(ppid, (first + 1) as u16))
    &&& (len < 3 || reg_allowed_spec(ppid, (first + 2) as u16))
    &&& (len < 4 || reg_allowed_spec(ppid, (first + 3) as u16))
}

} // verus!
