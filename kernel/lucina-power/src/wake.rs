// SPDX-License-Identifier: MPL-2.0

//! What Juno's wake interrupts need besides the PMIC's registers (`lucina_spmi::{irq, alarm}`): the PDC (its proved arithmetic is
//! in [`wake_rules`](crate::wake_rules)), which PDC pin a GPIO is, the GIC's side of a PDC pin, why the machine woke, and the RTC
//! class's `wakealarm` file.

pub use crate::wake_rules::{
    CFG_ENABLE, IRQ_ENABLE_BANK, IRQ_I_CFG, PDC_VERSION_3_2, PDC_VERSION_REG, PdcType,
    alarm_counter, cfg_offset, dual_edge_next, enable_reg, with_enable, with_type,
};

/// SC7180's GPIOs that reach the PDC, and their pins: `sc7180_pdc_map`, Linux 7.1 `drivers/pinctrl/qcom/pinctrl-sc7180.c:1111-1123`,
/// in its order.
pub const SC7180_PDC_MAP: &[(u32, u32)] = &[
    (0, 40),
    (3, 50),
    (4, 42),
    (5, 70),
    (6, 41),
    (9, 35),
    (10, 80),
    (11, 51),
    (16, 20),
    (21, 55),
    (22, 90),
    (23, 21),
    (24, 61),
    (26, 52),
    (28, 36),
    (30, 100),
    (31, 33),
    (32, 81),
    (33, 62),
    (34, 43),
    (36, 91),
    (37, 53),
    (38, 63),
    (39, 72),
    (41, 101),
    (42, 7),
    (43, 34),
    (45, 73),
    (47, 82),
    (49, 17),
    (52, 109),
    (53, 102),
    (55, 92),
    (56, 56),
    (57, 57),
    (58, 83),
    (59, 37),
    (62, 110),
    (63, 111),
    (64, 74),
    (65, 44),
    (66, 93),
    (67, 58),
    (68, 112),
    (69, 32),
    (70, 54),
    (72, 59),
    (73, 64),
    (74, 71),
    (78, 31),
    (82, 30),
    (85, 103),
    (86, 38),
    (87, 39),
    (88, 45),
    (89, 46),
    (90, 47),
    (91, 48),
    (92, 60),
    (93, 49),
    (94, 84),
    (95, 94),
    (98, 65),
    (101, 66),
    (104, 67),
    (109, 104),
    (110, 68),
    (113, 69),
    (114, 113),
    (115, 108),
    (116, 121),
    (117, 114),
    (118, 119),
];

/// The PDC pin of a GPIO, if it has one.
pub fn pdc_pin_of_gpio(gpio: u32) -> Option<u32> {
    SC7180_PDC_MAP
        .iter()
        .find(|(g, _)| *g == gpio)
        .map(|(_, pin)| *pin)
}

/// The GIC's trigger for a PDC pin's type: the PDC turns falling, rising and both edges into a rising edge, and either level into
/// a high level (`qcom_pdc_gic_set_type`, `qcom-pdc.c:185-203`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GicTrigger {
    EdgeRising,
    LevelHigh,
}

pub fn gic_trigger(t: PdcType) -> GicTrigger {
    match t {
        PdcType::EdgeFalling | PdcType::EdgeRising | PdcType::EdgeDual => GicTrigger::EdgeRising,
        PdcType::LevelLow | PdcType::LevelHigh => GicTrigger::LevelHigh,
    }
}

/// The GIC SPI of a PDC pin, from the device tree's `qcom,pdc-ranges` triples (pin base, SPI base, count): `pdc_pin_to_parent`
/// (`qcom-pdc.c:47`, `:299`).
pub fn gic_spi(pin: u32, ranges: &[(u32, u32, u32)]) -> Option<u32> {
    ranges
        .iter()
        .find(|(base, _, count)| pin >= *base && pin - *base < *count)
        .map(|(base, spi, _)| spi + (pin - base))
}

/// What woke the last sleep.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WakeReason {
    PowerKey,
    Lid,
    RtcAlarm,
    /// The shell's unblank (`FBIOBLANK`).
    Shell,
    /// The keyboard's interrupt line (`ECKB`, `GpioInt ... SharedAndWake`).
    Keyboard,
    /// The touchpad's interrupt line (`TCPD`, `GpioInt ... SharedAndWake`).
    Touchpad,
    /// A sleep test's dead-man timer (`sleep-test <seconds>` in `/proc/lucina-power`, patch 1345).
    Deadman,
}

impl WakeReason {
    pub fn name(self) -> &'static str {
        match self {
            WakeReason::PowerKey => "power key",
            WakeReason::Lid => "lid",
            WakeReason::RtcAlarm => "rtc alarm",
            WakeReason::Shell => "shell",
            WakeReason::Keyboard => "keyboard",
            WakeReason::Touchpad => "touchpad",
            WakeReason::Deadman => "deadman",
        }
    }

    /// Whether user space hears of this wake only through `KEY_WAKEUP`: the power key sends `KEY_POWER` and the lid `SW_LID`,
    /// and the shell's own unblank needs no telling.
    pub fn needs_key_wakeup(self) -> bool {
        matches!(
            self,
            WakeReason::RtcAlarm
                | WakeReason::Keyboard
                | WakeReason::Touchpad
                | WakeReason::Deadman
        )
    }
}

/// What a write to `wakealarm` does to the alarm.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlarmChange {
    /// Turned off: the RTC core removes its alarm timer (`rtc_timer_remove`), which turns the alarm off and clears it.
    Off,
    /// Set and on, at this time in seconds since the epoch.
    At(i64),
}

/// Why a write to `wakealarm` was refused, as Linux's error numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlarmError {
    /// `kstrtos64` refused the number.
    Invalid,
    /// An alarm is already on and the write was neither "+=" nor a time in the past (`-EBUSY`).
    Busy,
    /// "+=" with no alarm on to push (`-EINVAL`).
    NothingToPush,
    /// The sum overflowed.
    Range,
}

/// Linux's `wakealarm_store` (`drivers/rtc/sysfs.c:148-210`): `now` is the RTC's time, `pending` the alarm that is on, if any.
/// "+N" is N seconds from now, "+=N" pushes the alarm that is on by N, a plain number is a time; a time not in the future turns the
/// alarm off (Linux then writes a disabled alarm at now + 300, which the core turns into removing its timer). The checks in
/// Linux's order.
pub fn wakealarm_store(
    text: &str,
    now: i64,
    pending: Option<i64>,
) -> Result<AlarmChange, AlarmError> {
    let mut rest = text;
    let (mut push, mut adjust) = (false, false);
    if let Some(after_plus) = rest.strip_prefix('+') {
        if let Some(after_eq) = after_plus.strip_prefix('=') {
            rest = after_eq;
            push = true;
        } else {
            rest = after_plus;
            adjust = true;
        }
    }
    let mut alarm = kstrtos64(rest).ok_or(AlarmError::Invalid)?;
    if adjust {
        alarm = alarm.checked_add(now).ok_or(AlarmError::Range)?;
    }
    if alarm > now || push {
        match pending {
            Some(at) if push => alarm = alarm.checked_add(at).ok_or(AlarmError::Range)?,
            Some(_) => return Err(AlarmError::Busy),
            None if push => return Err(AlarmError::NothingToPush),
            None => {}
        }
        Ok(AlarmChange::At(alarm))
    } else {
        Ok(AlarmChange::Off)
    }
}

/// `kstrtos64(s, 0, ...)`: an optional sign, then decimal, `0x` hex or `0` octal digits, and one trailing newline at most.
fn kstrtos64(s: &str) -> Option<i64> {
    let s = s.strip_suffix('\n').unwrap_or(s);
    let (negative, digits) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let (radix, digits) = if let Some(hex) = digits
        .strip_prefix("0x")
        .or_else(|| digits.strip_prefix("0X"))
    {
        (16, hex)
    } else if digits.len() > 1 && digits.starts_with('0') {
        (8, &digits[1..])
    } else {
        (10, digits)
    };
    if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
        return None;
    }
    let magnitude = u64::from_str_radix(digits, radix).ok()?;
    if negative {
        0i64.checked_sub_unsigned(magnitude)
    } else {
        i64::try_from(magnitude).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lid_gpio_is_pdc_pin_49() {
        assert_eq!(pdc_pin_of_gpio(93), Some(49));
        assert_eq!(pdc_pin_of_gpio(1), None);
        assert_eq!(SC7180_PDC_MAP.len(), 73);
    }

    #[test]
    fn pdc_pins_map_to_the_laptops_spis() {
        let ranges = [(0, 480, 94), (94, 609, 31), (125, 63, 1)];
        assert_eq!(gic_spi(1, &ranges), Some(481));
        assert_eq!(gic_spi(49, &ranges), Some(529));
        assert_eq!(gic_spi(93, &ranges), Some(573));
        assert_eq!(gic_spi(94, &ranges), Some(609));
        assert_eq!(gic_spi(125, &ranges), Some(63));
        assert_eq!(gic_spi(126, &ranges), None);
    }

    #[test]
    fn the_gic_sees_rising_edges_and_high_levels() {
        assert_eq!(gic_trigger(PdcType::EdgeFalling), GicTrigger::EdgeRising);
        assert_eq!(gic_trigger(PdcType::EdgeDual), GicTrigger::EdgeRising);
        assert_eq!(gic_trigger(PdcType::LevelLow), GicTrigger::LevelHigh);
        assert_eq!(gic_trigger(PdcType::LevelHigh), GicTrigger::LevelHigh);
    }

    #[test]
    fn the_pdc_words() {
        assert_eq!(cfg_offset(1), 0x114);
        assert_eq!(cfg_offset(49), 0x1d4);
        assert_eq!(with_type(0xffff_fff8, PdcType::LevelHigh), 0xffff_fffc);
        assert_eq!(with_type(0x0000_0007, PdcType::LevelLow), 0);
        assert_eq!(enable_reg(0x30100, 49), 0x14);
        assert_eq!(enable_reg(0x30200, 49), 0x1d4);
        assert_eq!(with_enable(0x30100, 49, 0, true), 1 << 17);
        assert_eq!(with_enable(0x30200, 49, 0x4, true), 0xc);
        assert_eq!(with_enable(0x30200, 49, 0xc, false), 0x4);
    }

    #[test]
    fn only_wakes_without_their_own_event_send_key_wakeup() {
        assert!(WakeReason::RtcAlarm.needs_key_wakeup());
        assert!(WakeReason::Keyboard.needs_key_wakeup());
        assert!(WakeReason::Touchpad.needs_key_wakeup());
        assert!(!WakeReason::PowerKey.needs_key_wakeup());
        assert!(!WakeReason::Lid.needs_key_wakeup());
        assert!(!WakeReason::Shell.needs_key_wakeup());
        assert!(WakeReason::Deadman.needs_key_wakeup());
        assert_eq!(WakeReason::Deadman.name(), "deadman");
        assert_eq!(WakeReason::Keyboard.name(), "keyboard");
    }

    #[test]
    fn the_keyboard_and_touchpad_gpios_are_pdc_pins_67_and_33() {
        assert_eq!(pdc_pin_of_gpio(104), Some(67));
        assert_eq!(pdc_pin_of_gpio(31), Some(33));
        let ranges = [(0, 480, 94), (94, 609, 31), (125, 63, 1)];
        assert_eq!(gic_spi(67, &ranges), Some(547));
        assert_eq!(gic_spi(33, &ranges), Some(513));
        assert_eq!(gic_trigger(PdcType::LevelLow), GicTrigger::LevelHigh);
    }

    #[test]
    fn the_lid_waits_for_the_edge_away_from_its_level() {
        assert_eq!(dual_edge_next(true), PdcType::EdgeFalling);
        assert_eq!(dual_edge_next(false), PdcType::EdgeRising);
    }

    #[test]
    fn the_alarm_counter_is_the_time_less_the_offset() {
        assert_eq!(
            alarm_counter(1_800_000_030, 1_700_000_000),
            Some(100_000_030)
        );
        assert_eq!(alarm_counter(5, 10), None);
        assert_eq!(alarm_counter(i64::MAX, 0), None);
        assert_eq!(alarm_counter(i64::MIN, i64::MAX), None);
    }

    #[test]
    fn wakealarm_writes_as_linux_takes_them() {
        let now = 1000;
        // "+30" as `echo +30 >` writes it.
        assert_eq!(
            wakealarm_store("+30\n", now, None),
            Ok(AlarmChange::At(1030))
        );
        assert_eq!(
            wakealarm_store("2000", now, None),
            Ok(AlarmChange::At(2000))
        );
        assert_eq!(wakealarm_store("0", now, Some(1500)), Ok(AlarmChange::Off));
        assert_eq!(wakealarm_store("1000", now, None), Ok(AlarmChange::Off));
        assert_eq!(
            wakealarm_store("+30", now, Some(1500)),
            Err(AlarmError::Busy)
        );
        assert_eq!(
            wakealarm_store("+=30", now, Some(1500)),
            Ok(AlarmChange::At(1530))
        );
        assert_eq!(
            wakealarm_store("+=30", now, None),
            Err(AlarmError::NothingToPush)
        );
        assert_eq!(
            wakealarm_store("+=0", now, Some(1500)),
            Ok(AlarmChange::At(1500))
        );
        assert_eq!(wakealarm_store("0x10", 0, None), Ok(AlarmChange::At(16)));
        assert_eq!(wakealarm_store("010", 0, None), Ok(AlarmChange::At(8)));
        assert_eq!(wakealarm_store("-5", now, None), Ok(AlarmChange::Off));
        assert_eq!(wakealarm_store("soon", now, None), Err(AlarmError::Invalid));
        assert_eq!(wakealarm_store("", now, None), Err(AlarmError::Invalid));
        assert_eq!(
            wakealarm_store("+9223372036854775807", now, None),
            Err(AlarmError::Range)
        );
    }
}
