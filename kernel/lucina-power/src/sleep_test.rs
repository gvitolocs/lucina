// SPDX-License-Identifier: MPL-2.0

//! The parts of a sleep test and of the RTC alarm's setup that need no I/O (patch 1345): the `sleep-test <seconds>` word of
//! `/proc/lucina-power`, where the alarm's registers are (`lucina.rtc.alarm=dt|linux`), and which interrupts Linux makes wake
//! the SoC through the PDC on this laptop, against the ones routed here.
//!
//! The originals are Linux v6.12: `arch/arm64/boot/dts/qcom/pm6150.dtsi`, `sc7180.dtsi`, `drivers/rtc/rtc-pm8xxx.c`,
//! `drivers/irqchip/qcom-pdc.c`, `drivers/pinctrl/qcom/pinctrl-sc7180.c`, and the drivers named in [`LINUX_PDC_WAKE`]. The
//! laptop's own tree is the one it boots with (`model = "Samsung Galaxy Book Go"`).

use core::ops::RangeInclusive;

use crate::wake_rules::PdcType;

/// The seconds a sleep test may last: at least 5, so the sleep path has time to finish before the dead-man fires, and at most
/// 30 minutes, so a test that goes wrong comes back the same session.
pub const SLEEP_TEST_SECONDS: RangeInclusive<u32> = 5..=1800;

/// How much later than the dead-man timer the RTC alarm is armed, as its second line.
pub const ALARM_AFTER_DEADMAN_S: u32 = 5;

/// `sleep-test <seconds>` as written to `/proc/lucina-power` (one trailing newline and surrounding blanks allowed). `None`
/// when it is not that word, or the seconds are outside [`SLEEP_TEST_SECONDS`]: the write is then refused with EINVAL.
pub fn parse_sleep_test(text: &str) -> Option<u32> {
    let rest = text.trim().strip_prefix("sleep-test")?;
    let digits = rest
        .strip_prefix(|c: char| c.is_ascii_whitespace())?
        .trim_start();
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let seconds: u32 = digits.parse().ok()?;
    SLEEP_TEST_SECONDS.contains(&seconds).then_some(seconds)
}

/// The alarm's counter value for the second line: the counter now plus the test's seconds plus [`ALARM_AFTER_DEADMAN_S`].
/// The alarm compares against the same counter (`pm8xxx_rtc_set_alarm` writes `secs - offset`; here the offset is 0, since
/// the time is counted on the counter itself). `None` if it would wrap.
pub fn second_line_counter(counter_now: u32, seconds: u32) -> Option<u32> {
    counter_now
        .checked_add(seconds)?
        .checked_add(ALARM_AFTER_DEADMAN_S)
}

/// The alarm peripheral's base on PM6150: `pm6150.dtsi:145-151`, `pm6150_rtc: rtc@6000`, `reg = <0x6000>, <0x6100>`,
/// `reg-names = "rtc", "alarm"`. The same as `rtc-pm8xxx.c:429-437` `pm8941_regs` (`alarm_rw = 0x6140`, `alarm_ctrl =
/// 0x6146`, `alarm_ctrl2 = 0x6148`: base 0x6100 plus `lucina_spmi::alarm`'s offsets).
pub const PM6150_ALARM_BASE: u16 = 0x6100;

/// Where the alarm's base comes from (`lucina.rtc.alarm`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlarmBaseFrom {
    /// The device tree's `qcom,pm8941-rtc` "alarm" reg, by `reg-names` ([`child_reg_by_name`]; the default).
    Dt,
    /// Linux's `pm6150.dtsi`: [`PM6150_ALARM_BASE`].
    Linux,
}

/// `lucina.rtc.alarm`'s value: absent or `dt` is [`AlarmBaseFrom::Dt`], `linux` is [`AlarmBaseFrom::Linux`]; anything else is
/// `None`, and the caller keeps the default and says so.
pub fn alarm_base_from(arg: Option<&str>) -> Option<AlarmBaseFrom> {
    match arg {
        None | Some("dt") => Some(AlarmBaseFrom::Dt),
        Some("linux") => Some(AlarmBaseFrom::Linux),
        Some(_) => None,
    }
}

/// Why no alarm base was chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlarmBaseRefused {
    /// `dt`, and the tree's "alarm" reg did not read (the text is the one the boot log has always printed).
    NotInTree,
    /// `linux`, and the tree names another base for its alarm: the tree is not pm6150's, so the alarm is left alone.
    TreeDisagrees(u16),
}

impl AlarmBaseRefused {
    pub fn text(self) -> &'static str {
        match self {
            AlarmBaseRefused::NotInTree => "no qcom,pm8941-rtc alarm reg in the device tree",
            AlarmBaseRefused::TreeDisagrees(_) => {
                "the device tree's qcom,pm8941-rtc alarm reg is not pm6150.dtsi's 0x6100"
            }
        }
    }
}

/// The alarm's base. `dt_reg` is the tree's "alarm" reg ([`child_reg_by_name`]), if it names one: `dt` takes it, and `linux`
/// takes pm6150.dtsi's 0x6100 unless the tree names another.
pub fn alarm_base(from: AlarmBaseFrom, dt_reg: Option<u16>) -> Result<u16, AlarmBaseRefused> {
    match from {
        AlarmBaseFrom::Dt => dt_reg.ok_or(AlarmBaseRefused::NotInTree),
        AlarmBaseFrom::Linux => match dt_reg {
            Some(base) if base != PM6150_ALARM_BASE => Err(AlarmBaseRefused::TreeDisagrees(base)),
            _ => Ok(PM6150_ALARM_BASE),
        },
    }
}

/// The address of the entry of a `reg` property named `name` in its `reg-names`, for a node whose parent gives
/// `address_cells` and `size_cells` (`#address-cells`, `#size-cells`). A PMIC peripheral under an SPMI PMIC has one address
/// cell and no size cell (`pm6150.dtsi`: `pmic@0` `#address-cells = <1>`, `#size-cells = <0>`; `rtc@6000`:
/// `reg = <0x6000>, <0x6100>`, `reg-names = "rtc", "alarm"`), so each entry is one cell and the second is the alarm. Linux
/// reads such a node the same way, entry by entry: `of_property_read_u32_index(np, "reg", i, ...)`
/// (`drivers/usb/typec/tcpm/qcom/qcom_pmic_typec.c:65`, `:75`), and `of_property_read_u32(np, "reg", ...)` for the first
/// (`drivers/power/reset/qcom-pon.c:65`, `drivers/leds/rgb/leds-qcom-lpg.c:1348`). `reg` and `reg_names` are the raw
/// property values (big-endian cells; NUL-separated strings). `None` if the name is not there, the entry is past the end,
/// or the cells are not 1 or 2 for the address and 0 to 2 for the size.
pub fn child_reg_by_name(
    reg: &[u8],
    reg_names: &[u8],
    address_cells: usize,
    size_cells: usize,
    name: &str,
) -> Option<u64> {
    if !(1..=2).contains(&address_cells) || size_cells > 2 {
        return None;
    }
    let names = reg_names.strip_suffix(&[0]).unwrap_or(reg_names);
    let index = names
        .split(|&b| b == 0)
        .position(|n| n == name.as_bytes())?;
    let stride = 4 * (address_cells + size_cells);
    let entry = reg.get(index * stride..index * stride + 4 * address_cells)?;
    Some(entry.as_chunks::<4>().0.iter().fold(0u64, |acc, c| {
        acc << 32 | u64::from(u32::from_be_bytes([c[0], c[1], c[2], c[3]]))
    }))
}

/// The PPID of the alarm peripheral (SID 0, peripheral 0x61), as the arbiter's map holds it.
pub const ALARM_PPID: u16 = PM6150_ALARM_BASE >> 8;

/// The PPID in an entry of the arbiter's map (`SPMI_MAPPING_TABLE_REG`, bits 8 to 19: `spmi-pmic-arb.c`
/// `_pmic_arb_read_apid_map`, `ppid = (regval >> 8) & PMIC_ARB_PPID_MASK`).
pub fn map_entry_ppid(entry: u32) -> u16 {
    ((entry >> 8) & 0xfff) as u16
}

/// Where a wake line comes in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Line {
    /// A PDC pin named directly in the tree (`<&pdc pin type>`).
    Pdc(u32),
    /// A TLMM GPIO, which reaches the PDC through `tlmm`'s `wakeup-parent = <&pdc>` and `sc7180_pdc_map`.
    Gpio(u32),
}

/// What is done here with an interrupt Linux makes a wake source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Here {
    /// Routed and armed the same way (this PDC type).
    Routed(PdcType),
    /// Not routed, and why.
    NotRouted(&'static str),
}

/// An interrupt that Linux makes wake the SoC through the PDC on this laptop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LinuxWake {
    pub what: &'static str,
    pub line: Line,
    /// The PDC type Linux arms it with (`qcom_pdc_gic_set_type` takes the tree's type, or the driver's at suspend).
    pub linux_type: PdcType,
    /// Where Linux makes it a wake source.
    pub cite: &'static str,
    pub here: Here,
}

/// Why the USB PHY lines are not routed.
const NO_USB: &str =
    "no USB host driver here; and PDC pins 6, 8, 9 are outside lucina_sleep::pdc_write_allowed";

/// Every interrupt Linux v6.12 makes a wake source through the PDC with the laptop's tree, and what is done with it here.
/// Linux arms a wake interrupt only from a driver: `enable_irq_wake` or `dev_pm_set_wake_irq` / `dev_pm_set_dedicated_wake_irq`
/// (armed by `dpm_suspend_noirq`), and the PDC chip enables the pin then (`qcom-pdc.c:202-205`,
/// `IRQCHIP_ENABLE_WAKEUP_ON_SUSPEND`). Not in the list because Linux does not make them wake sources here: the GPU and Venus
/// (no wake call in `msm_gpu` or `venus`), the lid (`gpio-keys` `switch-lid` has no `wakeup-source`, `gpio_keys.c:812-815`;
/// it is armed here from Samsung's DSDT), `resin` (`status = "disabled"`), and the PD controllers' and the EC's lines (no node in
/// the tree; armed here from the DSDT).
pub const LINUX_PDC_WAKE: &[LinuxWake] = &[
    LinuxWake {
        what: "SPMI arbiter summary (power key PON irq 0, RTC alarm 0x61 irq 1)",
        line: Line::Pdc(1),
        linux_type: PdcType::LevelHigh,
        cite: "sc7180.dtsi:3518 `interrupts-extended = <&pdc 1 IRQ_TYPE_LEVEL_HIGH>`; pm8941-pwrkey.c:406, :226-227; \
               rtc-pm8xxx.c:506, :526 (the rtc is enabled on the sibling sc7180-acer-aspire1.dts:506-508)",
        here: Here::Routed(PdcType::LevelHigh),
    },
    LinuxWake {
        what: "keyboard (i2c-hid keyboard@5)",
        line: Line::Gpio(104),
        linux_type: PdcType::LevelLow,
        cite: "laptop tree `keyboard@5`: `interrupts-extended = <&tlmm 104 IRQ_TYPE_LEVEL_LOW>`, `wakeup-source`; \
               i2c-core-of.c:58, i2c-core-base.c:555, :560; pinctrl-sc7180.c:1125 {104, 67}",
        here: Here::Routed(PdcType::LevelLow),
    },
    LinuxWake {
        what: "touchpad (i2c-hid touchpad@40)",
        line: Line::Gpio(31),
        linux_type: PdcType::LevelLow,
        cite: "laptop tree `touchpad@40`: `interrupts-extended = <&tlmm 31 IRQ_TYPE_LEVEL_LOW>`, `wakeup-source`; \
               i2c-core-of.c:58, i2c-core-base.c:555, :560; pinctrl-sc7180.c:1117 {31, 33}",
        here: Here::Routed(PdcType::LevelLow),
    },
    LinuxWake {
        what: "USB dp_hs_phy_irq",
        line: Line::Pdc(9),
        // Typed at suspend: rising with nothing attached, falling with a full- or high-speed device.
        linux_type: PdcType::EdgeRising,
        cite: "sc7180.dtsi:3042 `<&pdc 9 IRQ_TYPE_EDGE_BOTH>`, :3060 `wakeup-source`, laptop tree `dr_mode = \"host\"`; \
               dwc3-qcom.c:817-818, :451-455, :393-404",
        here: Here::NotRouted(NO_USB),
    },
    LinuxWake {
        what: "USB dm_hs_phy_irq",
        line: Line::Pdc(8),
        linux_type: PdcType::EdgeRising,
        cite: "sc7180.dtsi:3043 `<&pdc 8 IRQ_TYPE_EDGE_BOTH>`; dwc3-qcom.c:817-818, :451-455, :393-404",
        here: Here::NotRouted(NO_USB),
    },
    LinuxWake {
        what: "USB ss_phy_irq",
        line: Line::Pdc(6),
        linux_type: PdcType::LevelHigh,
        cite: "sc7180.dtsi:3044 `<&pdc 6 IRQ_TYPE_LEVEL_HIGH>`; dwc3-qcom.c:817-818, :451-455, :407",
        here: Here::NotRouted(NO_USB),
    },
    LinuxWake {
        what: "Bluetooth UART RX wake (serial@88c000)",
        line: Line::Gpio(41),
        linux_type: PdcType::EdgeFalling,
        cite: "laptop tree `serial@88c000`: `interrupts-extended = <GIC 604>, <&tlmm 41 IRQ_TYPE_EDGE_FALLING>`; \
               qcom_geni_serial.c:1724, :1758-1761; pinctrl-sc7180.c:1119 {41, 101}",
        here: Here::NotRouted(
            "no Bluetooth UART driver here; and PDC pin 101 is outside lucina_sleep::pdc_write_allowed",
        ),
    },
];

/// A line Linux makes a wake source but which is routed here another way, or not at all while [`LINUX_PDC_WAKE`] says it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mismatch {
    pub what: &'static str,
    pub pin: u32,
    /// The type configured here, if the pin is routed here at all.
    pub here: Option<PdcType>,
}

/// Compares the lines routed here (PDC pin and type, as the wake setup configured them) with [`LINUX_PDC_WAKE`]: each line Linux
/// routes and the table marks as routed must be here with the same type. `pin_of_gpio` is `sc7180_pdc_map`.
pub fn routing_mismatches(
    routed: &[(u32, PdcType)],
    pin_of_gpio: impl Fn(u32) -> Option<u32>,
    out: &mut impl FnMut(Mismatch),
) {
    for wake in LINUX_PDC_WAKE {
        let Here::Routed(_) = wake.here else { continue };
        let pin = match wake.line {
            Line::Pdc(pin) => Some(pin),
            Line::Gpio(gpio) => pin_of_gpio(gpio),
        };
        let Some(pin) = pin else {
            out(Mismatch {
                what: wake.what,
                pin: u32::MAX,
                here: None,
            });
            continue;
        };
        let here = routed.iter().find(|(p, _)| *p == pin).map(|(_, t)| *t);
        if here != Some(wake.linux_type) {
            out(Mismatch {
                what: wake.what,
                pin,
                here,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use std::vec::Vec;

    use super::*;
    use crate::wake::pdc_pin_of_gpio;

    #[test]
    fn sleep_test_takes_5_to_1800_seconds() {
        assert_eq!(parse_sleep_test("sleep-test 5"), Some(5));
        assert_eq!(parse_sleep_test("sleep-test 1800\n"), Some(1800));
        assert_eq!(parse_sleep_test("  sleep-test   60 \n"), Some(60));
        assert_eq!(parse_sleep_test("sleep-test 4"), None);
        assert_eq!(parse_sleep_test("sleep-test 0"), None);
        assert_eq!(parse_sleep_test("sleep-test 1801"), None);
        assert_eq!(parse_sleep_test("sleep-test 99999999999"), None);
        assert_eq!(parse_sleep_test("sleep-test"), None);
        assert_eq!(parse_sleep_test("sleep-test "), None);
        assert_eq!(parse_sleep_test("sleep-test -5"), None);
        assert_eq!(parse_sleep_test("sleep-test +5"), None);
        assert_eq!(parse_sleep_test("sleep-test 5s"), None);
        assert_eq!(parse_sleep_test("sleep-test60"), None);
        assert_eq!(parse_sleep_test("sleep-tests 60"), None);
        assert_eq!(parse_sleep_test("press"), None);
    }

    #[test]
    fn the_second_line_is_five_seconds_after_the_dead_man() {
        assert_eq!(second_line_counter(1000, 60), Some(1065));
        assert_eq!(second_line_counter(u32::MAX - 10, 5), Some(u32::MAX));
        assert_eq!(second_line_counter(u32::MAX - 9, 5), None);
        assert_eq!(second_line_counter(u32::MAX, 5), None);
    }

    #[test]
    fn the_alarm_base_is_the_trees_by_default_and_pm6150s_with_linux() {
        assert_eq!(alarm_base_from(None), Some(AlarmBaseFrom::Dt));
        assert_eq!(alarm_base_from(Some("dt")), Some(AlarmBaseFrom::Dt));
        assert_eq!(alarm_base_from(Some("linux")), Some(AlarmBaseFrom::Linux));
        assert_eq!(alarm_base_from(Some("Linux")), None);
        assert_eq!(alarm_base_from(Some("")), None);
        // dt: the tree's reg, or the old refusal when it names none.
        assert_eq!(alarm_base(AlarmBaseFrom::Dt, Some(0x6100)), Ok(0x6100));
        assert_eq!(alarm_base(AlarmBaseFrom::Dt, Some(0x6200)), Ok(0x6200));
        assert_eq!(
            alarm_base(AlarmBaseFrom::Dt, None),
            Err(AlarmBaseRefused::NotInTree)
        );
        assert_eq!(
            AlarmBaseRefused::NotInTree.text(),
            "no qcom,pm8941-rtc alarm reg in the device tree"
        );
        // linux: pm6150.dtsi's base when the tree omits it or agrees; never over a tree that says otherwise.
        assert_eq!(alarm_base(AlarmBaseFrom::Linux, None), Ok(0x6100));
        assert_eq!(alarm_base(AlarmBaseFrom::Linux, Some(0x6100)), Ok(0x6100));
        assert_eq!(
            alarm_base(AlarmBaseFrom::Linux, Some(0x6200)),
            Err(AlarmBaseRefused::TreeDisagrees(0x6200))
        );
        assert_eq!(PM6150_ALARM_BASE, 0x6100);
        assert_eq!(ALARM_PPID, 0x061);
    }

    #[test]
    fn the_alarm_base_agrees_with_rtc_pm8xxx_and_the_write_allow_list() {
        // pm8941_regs: alarm_rw 0x6140, alarm_ctrl 0x6146, alarm_ctrl2 0x6148 (rtc-pm8xxx.c:433-435).
        assert_eq!(PM6150_ALARM_BASE + lucina_alarm::ALARM_RW, 0x6140);
        assert_eq!(PM6150_ALARM_BASE + lucina_alarm::ALARM_CTRL, 0x6146);
        assert_eq!(PM6150_ALARM_BASE + lucina_alarm::ALARM_CTRL2, 0x6148);
    }

    /// `lucina_spmi::alarm`'s offsets, restated: this crate does not depend on lucina-spmi.
    mod lucina_alarm {
        pub const ALARM_RW: u16 = 0x40;
        pub const ALARM_CTRL: u16 = 0x46;
        pub const ALARM_CTRL2: u16 = 0x48;
    }

    /// Big-endian cells, as a `reg` property holds them.
    fn cells(values: &[u32]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_be_bytes()).collect()
    }

    #[test]
    fn pmic_children_have_one_cell_per_reg_entry() {
        // pm6150.dtsi's rtc@6000 under pmic@0 (#address-cells = <1>, #size-cells = <0>), and the laptop's tree, the same.
        let reg = cells(&[0x6000, 0x6100]);
        let names = b"rtc\0alarm\0";
        // (address cells, size cells, name) -> address
        let table: &[(usize, usize, &str, Option<u64>)] = &[
            (1, 0, "rtc", Some(0x6000)),
            (1, 0, "alarm", Some(0x6100)),
            (1, 0, "nope", None),
            // Read as an (address, size) pair, as the old reader did, the alarm is lost: one entry, 0x6000 sized 0x6100.
            (1, 1, "rtc", Some(0x6000)),
            (1, 1, "alarm", None),
            // The fdt crate's defaults (2, 1) for a node whose parent it does not know: nothing that makes sense.
            (2, 1, "alarm", None),
            (3, 0, "rtc", None),
            (0, 0, "rtc", None),
            (1, 3, "rtc", None),
        ];
        for &(a, sz, name, want) in table {
            assert_eq!(
                child_reg_by_name(&reg, names, a, sz, name),
                want,
                "{} {} {}",
                a,
                sz,
                name
            );
        }
        // Without the trailing NUL, and one entry only (pm6150.dtsi's pon@800: `reg = <0x800>`).
        assert_eq!(
            child_reg_by_name(&reg, b"rtc\0alarm", 1, 0, "alarm"),
            Some(0x6100)
        );
        assert_eq!(
            child_reg_by_name(&cells(&[0x800]), b"pon\0", 1, 0, "pon"),
            Some(0x800)
        );
        // A memory-mapped node (2, 2), such as the arbiter's own "chnls" block: the second entry's 64-bit address.
        let arb = cells(&[0, 0xc440000, 0, 0x1100, 0, 0xc600000, 0, 0x2000000]);
        assert_eq!(
            child_reg_by_name(&arb, b"core\0chnls\0", 2, 2, "chnls"),
            Some(0xc600000)
        );
        assert_eq!(child_reg_by_name(&[], b"rtc\0", 1, 0, "rtc"), None);
    }

    #[test]
    fn map_entries_carry_the_ppid_in_bits_8_to_19() {
        assert_eq!(map_entry_ppid(0x0000_6100), 0x061);
        assert_eq!(map_entry_ppid(0x0100_0800 | 0xff), 0x008);
        assert_eq!(map_entry_ppid(0xfff0_0000 | (0x561 << 8)), 0x561);
    }

    /// The lines the wake setup routes (`qcom_wake.rs`): the SPMI summary, the keyboard and the touchpad as Linux does, and the
    /// lid, the PD controllers and Samsung's EC from the DSDT.
    const ROUTED_HERE: &[(u32, PdcType)] = &[
        (1, PdcType::LevelHigh),
        (49, PdcType::EdgeRising),
        (67, PdcType::LevelLow),
        (33, PdcType::LevelLow),
        (50, PdcType::LevelLow),
        (42, PdcType::LevelLow),
        (112, PdcType::EdgeFalling),
    ];

    #[test]
    fn every_line_linux_routes_here_is_routed_the_same_way() {
        let mut found = Vec::new();
        routing_mismatches(ROUTED_HERE, pdc_pin_of_gpio, &mut |m| found.push(m));
        assert_eq!(found, Vec::new());
    }

    #[test]
    fn a_line_left_out_or_typed_otherwise_is_a_mismatch() {
        let mut found = Vec::new();
        let without_touchpad = [(1, PdcType::LevelHigh), (67, PdcType::LevelLow)];
        routing_mismatches(&without_touchpad, pdc_pin_of_gpio, &mut |m| found.push(m));
        assert_eq!(
            found,
            [Mismatch {
                what: "touchpad (i2c-hid touchpad@40)",
                pin: 33,
                here: None
            }]
        );
        found.clear();
        let edge_spmi = [
            (1, PdcType::EdgeRising),
            (67, PdcType::LevelLow),
            (33, PdcType::LevelLow),
        ];
        routing_mismatches(&edge_spmi, pdc_pin_of_gpio, &mut |m| found.push(m));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].pin, 1);
        assert_eq!(found[0].here, Some(PdcType::EdgeRising));
    }

    #[test]
    fn the_linux_table_is_the_cited_list() {
        let pins: Vec<u32> = LINUX_PDC_WAKE
            .iter()
            .map(|w| match w.line {
                Line::Pdc(pin) => pin,
                Line::Gpio(gpio) => pdc_pin_of_gpio(gpio).expect("a wake GPIO has a PDC pin"),
            })
            .collect();
        assert_eq!(pins, [1, 67, 33, 9, 8, 6, 101]);
        // Those routed here are routed with Linux's own type; the others say why not.
        for w in LINUX_PDC_WAKE {
            match w.here {
                Here::Routed(t) => assert_eq!(t, w.linux_type, "{}", w.what),
                Here::NotRouted(why) => assert!(why.contains("pdc_write_allowed"), "{}", w.what),
            }
        }
        // Not invented: no GPU, Venus or lid line.
        assert!(
            LINUX_PDC_WAKE
                .iter()
                .all(|w| !w.what.contains("GPU") && !w.what.contains("Venus"))
        );
        assert!(LINUX_PDC_WAKE.iter().all(|w| w.line != Line::Gpio(93)));
    }
}
