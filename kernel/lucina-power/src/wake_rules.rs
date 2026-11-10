// SPDX-License-Identifier: MPL-2.0

//! The PDC's registers for the wake interrupts, the dual-edge rule for the lid, and the alarm's counter value, as plain arithmetic.
//! Verus proves this very file (`verification/juno-power/rules.rs`, D-011); what each function promises is in its `ensures`.
//!
//! The originals: Linux 7.1 `drivers/irqchip/qcom-pdc.c` (`IRQ_ENABLE_BANK` 0x10 `:27`, `IRQ_i_CFG` 0x110 `:29`,
//! `IRQ_i_CFG_IRQ_ENABLE` bit 3 `:32`, `PDC_VERSION_REG` 0x1000 `:36`, `PDC_VERSION_3_2` 0x30200 `:39`, the types `:161-165`,
//! `__pdc_enable_intr` `:100-122`, `qcom_pdc_gic_set_type` `:209-211`), `drivers/pinctrl/qcom/pinctrl-msm.c`
//! (`msm_gpio_update_dual_edge_parent` `:951-986`), and `drivers/rtc/rtc-pm8xxx.c` (`pm8xxx_rtc_set_alarm`: `secs - offset`).
#![allow(unexpected_cfgs)]

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const PDC_VERSION_REG: u32 = 0x1000;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const PDC_VERSION_3_2: u32 = 0x30200;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const IRQ_ENABLE_BANK: u32 = 0x10;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const IRQ_I_CFG: u32 = 0x110;
/// `IRQ_i_CFG_IRQ_ENABLE`: the enable bit in `IRQ_i_CFG` from version 3.2, bit 3 (written as its value, which Verus reads directly).
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const CFG_ENABLE: u32 = 0x8;
/// The PDC's pins (`PDC_MAX_IRQS`, 168 on these SoCs): offsets are only computed for pins below this.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const MAX_PIN: u32 = 168;

/// A pin's type in `IRQ_i_CFG` bits 2..0 (`pdc_irq_config_bits`, `:161-165`).
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PdcType {
    LevelLow,
    EdgeFalling,
    LevelHigh,
    EdgeRising,
    EdgeDual,
}

/// The three bits of a type.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == type_bits_spec(t),
        r <= 7,
))]
pub fn type_bits(t: PdcType) -> u32 {
    match t {
        PdcType::LevelLow => 0b000,
        PdcType::EdgeFalling => 0b010,
        PdcType::LevelHigh => 0b100,
        PdcType::EdgeRising => 0b110,
        PdcType::EdgeDual => 0b111,
    }
}

/// `IRQ_i_CFG` of `pin`.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires
        pin < MAX_PIN,
    ensures
        r == IRQ_I_CFG + 4 * pin,
))]
pub fn cfg_offset(pin: u32) -> u32 {
    IRQ_I_CFG + 4 * pin
}

/// `qcom_pdc_gic_set_type`: the type in bits 2..0, every other bit of the word kept (`rules.rs` proves both from this formula).
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == (cfg & !7u32) | type_bits_spec(t),
))]
pub fn with_type(cfg: u32, t: PdcType) -> u32 {
    (cfg & !7) | type_bits(t)
}

/// The word that holds `pin`'s enable: `IRQ_ENABLE_BANK` + 4 * (pin / 32) before version 3.2, its `IRQ_i_CFG` from then on.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires
        pin < MAX_PIN,
    ensures
        version < PDC_VERSION_3_2 ==> r == IRQ_ENABLE_BANK + 4 * (pin / 32),
        version >= PDC_VERSION_3_2 ==> r == IRQ_I_CFG + 4 * pin,
))]
pub fn enable_reg(version: u32, pin: u32) -> u32 {
    if version < PDC_VERSION_3_2 { IRQ_ENABLE_BANK + 4 * (pin / 32) } else { cfg_offset(pin) }
}

/// The enable word after `__pdc_enable_intr`: only `pin`'s bit (pin % 32 before 3.2, bit 3 from then on) set or cleared (`rules.rs`
/// proves that no other bit changes).
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        on ==> r == old | enable_bit_spec(version, pin),
        !on ==> r == old & !enable_bit_spec(version, pin),
))]
pub fn with_enable(version: u32, pin: u32, old: u32, on: bool) -> u32 {
    let bit = if version < PDC_VERSION_3_2 { 1u32 << (pin % 32) } else { CFG_ENABLE };
    if on { old | bit } else { old & !bit }
}

/// SC7180's dual-edge errata (`wakeirq_dual_edge_errata`, `pinctrl-sc7180.c:1139`): the PDC waits for the edge away from the level
/// the line has now (`msm_gpio_update_dual_edge_parent`): high waits for falling, low for rising.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        level_high ==> r == PdcType::EdgeFalling,
        !level_high ==> r == PdcType::EdgeRising,
))]
pub fn dual_edge_next(level_high: bool) -> PdcType {
    if level_high { PdcType::EdgeFalling } else { PdcType::EdgeRising }
}

/// The alarm's counter value for a time (`pm8xxx_rtc_set_alarm`: `secs -= rtc_dd->offset`), or none when it does not fit the 32
/// bits of `alarm_rw`.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r.is_some() ==> (r.unwrap() as int) + (offset as int) == (at as int),
        r.is_none() ==> ((at as int) - (offset as int)) < 0 || ((at as int) - (offset as int)) > (u32::MAX as int),
))]
pub fn alarm_counter(at: i64, offset: i64) -> Option<u32> {
    let diff = at as i128 - offset as i128;
    if diff < 0 || diff > u32::MAX as i128 { None } else { Some(diff as u32) }
}

/// Polls of the key and the lid after an interrupt, or while the key is down, before the power thread waits for the next
/// interrupt again (patch 1193; 1096 used it for the sleep only). Ten polls of 20 ms: the lid's debounce wants two equal samples
/// after its edge (`Lid::feed`), and a press that put the screen to sleep is still being released.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const QUIET_POLLS: u32 = 10;

/// Whether the power thread waits for an interrupt (the power key's PON edge, the alarm, the lid's primed edge, an input line
/// while asleep) instead of polling the key and the lid again. `armed`: every wake line is mapped and its peripheral unmasked
/// (`qcom_wake::armed`); `quiet`: polls since anything moved. Without the interrupts it polls, as before 1096
/// (`pm8941-pwrkey.c` and the lid's `gpio-keys` are interrupt-driven in Linux; the poll is only the fallback).
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == may_wait_spec(armed, quiet),
))]
pub fn may_wait(armed: bool, quiet: u32) -> bool {
    armed && quiet >= QUIET_POLLS
}

#[cfg(verus_keep_ghost)]
verus! {

pub open spec fn may_wait_spec(armed: bool, quiet: u32) -> bool {
    armed && quiet >= QUIET_POLLS
}

pub open spec fn type_bits_spec(t: PdcType) -> u32 {
    match t {
        PdcType::LevelLow => 0b000,
        PdcType::EdgeFalling => 0b010,
        PdcType::LevelHigh => 0b100,
        PdcType::EdgeRising => 0b110,
        PdcType::EdgeDual => 0b111,
    }
}

pub open spec fn enable_bit_spec(version: u32, pin: u32) -> u32 {
    if version < 0x30200 { (1u32 << (pin % 32)) as u32 } else { 8u32 }
}

} // verus!
