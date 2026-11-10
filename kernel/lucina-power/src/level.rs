//! Brightness levels and the PWM value they give, on the Galaxy Book Go. Proved by Verus in this very file
//! (`verification/juno-power/rules.rs`).
//!
//! The originals:
//! - Windows' panel table (`GPU0._ROM` in the DSDT): `BacklightSteps` 100, `BacklightPmicPWMSizeinBits` 9, `BacklightDefault` 80,
//!   `BacklightLowPower` 40. Windows counts brightness in levels 0 to 100.
//! - The firmware's own log: "SetBacklightByPWM: uLevel 00000100 Level 50", so level 50 is 0x100 of a 9-bit PWM, which is
//!   `level × 512 / 100`. That is the only point the originals give; the straight line through it and 0 is how this reads it.
//! - The PWM's size from the channel itself (`leds-qcom-lpg.c`: `PWM_SIZE_CLK` bit 2 selects 9 bits, else 6), and its top
//!   value `2^bits - 1` (`lpg_apply_pwm_value` caps the duty there).
//! - Linux's backlight class: brightness 0 to `max_brightness`, and at 0 the PWM is turned off rather than given a 0 duty
//!   (`pwm_bl.c`).
//!
//! Nothing here raises a limit: the top level is the PWM's top value, which the hardware has anyway; the firmware's level stays
//! until someone asks for another.
#![allow(unexpected_cfgs)]

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

/// Windows' `BacklightSteps`: levels 0 to 100.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const STEPS: u16 = 100;
/// Windows' `BacklightLowPower`, the level for low power.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const LOW_POWER: u16 = 40;

/// The top PWM value of a channel whose `PWM_SIZE_CLK` register reads `size_clk`: 511 for 9 bits, 63 for 6.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == 511 || r == 63,
        (size_clk & 4 != 0) == (r == 511),
))]
pub fn pwm_max(size_clk: u8) -> u16 {
    if size_clk & 4 != 0 { 511 } else { 63 }
}

/// The PWM value of `level` (0 to [`STEPS`], more is taken as [`STEPS`]): `level × (max + 1) / 100`, never above `max`.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires max <= 511,
    ensures
        r <= max,
        level == 0 ==> r == 0,
        level <= STEPS && (level as int) * (max as int + 1) / (STEPS as int) <= max as int
            ==> r as int == (level as int) * (max as int + 1) / (STEPS as int),
        level >= STEPS ==> r == max,
))]
pub fn value_for_level(level: u16, max: u16) -> u16 {
    let level = if level > STEPS { STEPS } else { level };
    #[cfg(verus_keep_ghost)]
    proof! { assert(level as u64 * (max as u64 + 1) <= 100 * 512) by (nonlinear_arith) requires level <= 100, max <= 511; }
    let value = level as u64 * (max as u64 + 1) / STEPS as u64;
    if value > max as u64 { max } else { value as u16 }
}

/// The level a PWM value stands for, rounded up so that every level reads back as itself: what the firmware left, as a level
/// (0x100 of 511 is 50).
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires max <= 511,
    ensures
        r <= STEPS,
        value >= max ==> r == STEPS,
))]
pub fn level_for_value(value: u16, max: u16) -> u16 {
    if value >= max {
        return STEPS;
    }
    #[cfg(verus_keep_ghost)]
    proof! { assert(value as u64 * 100 <= 511 * 100) by (nonlinear_arith) requires value <= 511; }
    let level = (value as u64 * STEPS as u64 + max as u64) / (max as u64 + 1);
    if level > STEPS as u64 { STEPS } else { level as u16 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_firmware_point() {
        assert_eq!(value_for_level(50, 511), 0x100);
        assert_eq!(level_for_value(0x100, 511), 50);
    }

    #[test]
    fn ends_and_clamps() {
        assert_eq!(value_for_level(0, 511), 0);
        assert_eq!(value_for_level(100, 511), 511);
        assert_eq!(value_for_level(250, 511), 511);
        assert_eq!(value_for_level(1, 511), 5);
        assert_eq!(value_for_level(40, 511), 204);
        assert_eq!(value_for_level(80, 511), 409);
        assert_eq!(level_for_value(511, 511), 100);
        assert_eq!(level_for_value(0, 511), 0);
        assert_eq!(value_for_level(100, 63), 63);
    }

    #[test]
    fn size_bit() {
        assert_eq!(pwm_max(0x07), 511);
        assert_eq!(pwm_max(0x03), 63);
    }

    #[test]
    fn every_level_round_trips_and_rises() {
        let mut last = 0;
        for level in 1..=100 {
            let v = value_for_level(level, 511);
            assert!(v > last, "level {level}");
            assert_eq!(level_for_value(v, 511), level, "level {level}");
            last = v;
        }
    }
}
