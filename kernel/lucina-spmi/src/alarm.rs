// SPDX-License-Identifier: MPL-2.0

//! The RTC alarm of `qcom,pm8941-rtc`: Linux's `drivers/rtc/rtc-pm8xxx.c` (7.1), `pm8941_regs` (`:535-543`). All addresses are
//! the alarm peripheral's base (the device tree's "alarm" reg, 0x6100 on PM6150) plus a register. "Update bits" below is
//! `regmap_update_bits`: read, change only the masked bits, write only when the value changed.

use crate::{
    Error,
    irq::{Periph, Trigger},
};

/// `alarm_rw`: the alarm, 4 bytes little endian, in counter seconds (the time less the RTC's offset).
pub const ALARM_RW: u16 = 0x40;
/// `alarm_ctrl` and its `alarm_en`.
pub const ALARM_CTRL: u16 = 0x46;
pub const ALARM_EN: u8 = 1 << 7;
/// `alarm_ctrl2` and `PM8xxx_RTC_ALARM_CLEAR`.
pub const ALARM_CTRL2: u16 = 0x48;
pub const ALARM_CLEAR: u8 = 1 << 0;
/// The alarm's interrupt in its peripheral, and its trigger (`pm6150.dtsi`: `interrupts = <0x0 0x61 0x1 IRQ_TYPE_EDGE_RISING>`).
pub const IRQ: u8 = 1;
pub const TRIGGER: Trigger = Trigger::EdgeRising;

/// `regmap_update_bits`: the bits of `mask` become those of `value`; nothing is written when they already are.
fn update_bits<P: Periph>(p: &mut P, addr: u16, mask: u8, value: u8) -> Result<(), Error> {
    let mut old = [0u8; 1];
    p.read(addr, &mut old)?;
    let new = (old[0] & !mask) | (value & mask);
    if new != old[0] {
        p.write(addr, &[new])?;
    }
    Ok(())
}

/// `pm8xxx_rtc_set_alarm` with `enabled` (`:384-411`): the alarm off, the 4 bytes written, the alarm on.
pub fn set<P: Periph>(p: &mut P, base: u16, counter: u32) -> Result<(), Error> {
    update_bits(p, base + ALARM_CTRL, ALARM_EN, 0)?;
    p.write(base + ALARM_RW, &counter.to_le_bytes())?;
    update_bits(p, base + ALARM_CTRL, ALARM_EN, ALARM_EN)
}

/// `pm8xxx_rtc_alarm_irq_enable(0)` (`:447-474`), which is what the RTC core does for an alarm turned off (`rtc_timer_remove` →
/// `rtc_alarm_disable`): the alarm off, then the 4 bytes cleared.
pub fn disable<P: Periph>(p: &mut P, base: u16) -> Result<(), Error> {
    update_bits(p, base + ALARM_CTRL, ALARM_EN, 0)?;
    p.write(base + ALARM_RW, &[0; 4])
}

/// `pm8xxx_alarm_trigger` (`:484-505`): the alarm off, then ALARM_CLEAR in `alarm_ctrl2` updated to 0, as Linux does it.
pub fn on_irq<P: Periph>(p: &mut P, base: u16) -> Result<(), Error> {
    update_bits(p, base + ALARM_CTRL, ALARM_EN, 0)?;
    update_bits(p, base + ALARM_CTRL2, ALARM_CLEAR, 0)
}

/// The alarm, in counter seconds, while it is on (`pm8xxx_rtc_read_alarm` reads the same two registers).
pub fn armed<P: Periph>(p: &mut P, base: u16) -> Result<Option<u32>, Error> {
    let mut ctrl = [0u8; 1];
    p.read(base + ALARM_CTRL, &mut ctrl)?;
    if ctrl[0] & ALARM_EN == 0 {
        return Ok(None);
    }
    let mut value = [0u8; 4];
    p.read(base + ALARM_RW, &mut value)?;
    Ok(Some(u32::from_le_bytes(value)))
}
