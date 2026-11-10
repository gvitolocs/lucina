//! The MAX77696 PMIC, as far as the panel needs it: its rails and the ADC of its thermistor.
//!
//! This and [`crate::off`] are the only ways to the PMIC in this crate, and [`Pmic::write`] refuses everything outside
//! [`WRITABLE`]. The PMIC also holds the registers that power the device off for good (`GLBLCNFG0`) and charge its
//! battery. The panel's other voltages (`EPDVEE`, `EPDVNEG`, `EPDVPOS`) and its sequence (`EPDSEQ`, `EPDDIS`) stay as
//! they are: no driver for this device has ever written them, and the panel runs on those values.
//!
//! VCOM and VDDH are checked before the rails go up, and only this panel's values can be written.

use crate::Error;

/// The PMIC's page on I2C1, for the kernel's side of [`PmicBus`].
pub const ADDR: u8 = 0x3c;

/// One register at a time on the PMIC.
pub trait PmicBus {
    fn read(&mut self, reg: u8) -> Result<u8, Error>;
    fn write(&mut self, reg: u8, val: u8) -> Result<(), Error>;
    /// Waits at least this long, without sleeping.
    fn delay_us(&mut self, us: u32);
}

/// The panel logic's 3.2 V: bit 0 switches it, the others stay.
const LSW4_CNTRL: u8 = 0x5f;
const LSW_OUTLS: u8 = 1 << 0;
/// The panel's high voltages and VCOM: bits 7 and 6 switch them, the soft-start bits stay.
const EPDCNFG: u8 = 0x60;
const EPDCNFG_EPDEN: u8 = 1 << 7;
const EPDCNFG_VCOMEN: u8 = 1 << 6;
/// The live state of the panel's rails. Any of bits 5:0 is a fault on one.
const EPDINTS: u8 = 0x61;
const EPDINTS_FAULTS: u8 = 0x3f;
const EPDVCOM: u8 = 0x64;
const EPDVCOMR: u8 = 0xb4;
const EPDVDDH: u8 = 0x68;

const ADCCNTL: u8 = 0x26;
const ADCCNTL_EN: u8 = 1 << 0;
const ADCCNTL_AVG32: u8 = 3 << 2;
const ADCCNTL_CONV: u8 = 1 << 4;
const ADCDLY: u8 = 0x27;
const ADCSEL0: u8 = 0x28;
const ADCSEL1: u8 = 0x29;
const ADCCHSEL: u8 = 0x2a;
const ADCDATAL: u8 = 0x2b;
const ADCDATAH: u8 = 0x2c;
const ADCICNFG: u8 = 0x30;
/// 50 uA through the thermistor.
const ADCICNFG_50UA: u8 = 3;

/// The frontlight: bit 7 switches its LED string, the other bits stay; the level has its high byte and its low nibble.
const LEDBST_CNTRL1: u8 = 0x6c;
const LED1EN: u8 = 1 << 7;
const LED1CURRENT_1: u8 = 0x6d;
const LED1CURRENT_2: u8 = 0x6e;
/// The frontlight's highest level.
pub const MAX_BRIGHTNESS: u16 = 4095;
/// The live state of the power key, in bit 7. Reading it clears nothing.
const GLBLSTAT: u8 = 0x07;
const GLBLSTAT_EN0: u8 = 1 << 7;

/// The panel's VCOM, -2.44 V in steps of 9.985 mV. It belongs to this panel: the panel's ROM says so under KindleOS.
pub const VCOM_CODE: u16 = 244;
/// VDDH, 24.87 V, the value for Carta panels. Asking for more than this is refused.
pub const VDDH_CODE: u8 = 21;

/// The only registers this crate may write.
pub const WRITABLE: [u8; 14] = [
    LSW4_CNTRL,
    EPDCNFG,
    EPDVCOM,
    EPDVCOMR,
    EPDVDDH,
    ADCCNTL,
    ADCDLY,
    ADCSEL0,
    ADCSEL1,
    ADCCHSEL,
    ADCICNFG,
    LEDBST_CNTRL1,
    LED1CURRENT_1,
    LED1CURRENT_2,
];

/// The thermistor's ADC code at 75 C, 74 C, down to -10 C.
const NTC_CODES: [u16; 86] = [
    0x095, 0x09a, 0x09f, 0x0a4, 0x0aa, 0x0af, 0x0b4, 0x0ba, 0x0bf, 0x0c5, 0x0ca, 0x0d0, 0x0d6,
    0x0dc, 0x0e3, 0x0e9, 0x0f0, 0x0f7, 0x0fe, 0x105, 0x10d, 0x114, 0x11d, 0x125, 0x12d, 0x136,
    0x140, 0x149, 0x153, 0x15d, 0x168, 0x172, 0x17e, 0x189, 0x195, 0x1a2, 0x1af, 0x1bc, 0x1ca,
    0x1d8, 0x1e6, 0x1f6, 0x205, 0x215, 0x226, 0x237, 0x249, 0x25b, 0x26e, 0x281, 0x295, 0x2a9,
    0x2be, 0x2d4, 0x2ea, 0x301, 0x319, 0x331, 0x34a, 0x363, 0x37e, 0x399, 0x3b4, 0x3d1, 0x3ee,
    0x40c, 0x42b, 0x44a, 0x46a, 0x48b, 0x4ad, 0x4d0, 0x4f3, 0x517, 0x53c, 0x562, 0x589, 0x5b1,
    0x5da, 0x603, 0x62e, 0x659, 0x686, 0x6b3, 0x6e1, 0x710,
];
const NTC_MAX_C: i32 = 75;

/// The physical ADC channels of the thermistor and its ground, with the current's mux for each.
const AIN0: (u8, u8) = (9, 0);
const AIN2: (u8, u8) = (12, 2);

/// The temperature of a thermistor code: the entry it is at, or the colder one around it.
fn ntc_temp(code: u16) -> i32 {
    let index = NTC_CODES
        .iter()
        .position(|&entry| code <= entry)
        .unwrap_or(NTC_CODES.len() - 1);
    NTC_MAX_C - index as i32
}

/// The rails' live faults, from `EPDINTS`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Faults(pub u8);

/// The PMIC. See the module's documentation for what it refuses.
pub struct Pmic<B> {
    bus: B,
}

impl<B: PmicBus> Pmic<B> {
    pub fn new(bus: B) -> Self {
        Self { bus }
    }

    /// Writes a register on the list.
    fn write(&mut self, reg: u8, val: u8) -> Result<(), Error> {
        let allowed = match reg {
            EPDVDDH => val == VDDH_CODE,
            EPDVCOM => val == VCOM_CODE as u8,
            // Only its lowest bit is VCOM's: the others stay as they are.
            EPDVCOMR => u16::from(val & 1) == VCOM_CODE >> 8,
            _ => WRITABLE.contains(&reg),
        };
        if !allowed {
            return Err(Error::PmicRefused { reg, val });
        }
        self.bus.write(reg, val)
    }

    fn update(&mut self, reg: u8, clear: u8, set: u8) -> Result<(), Error> {
        let val = self.bus.read(reg)?;
        self.write(reg, (val & !clear) | set)
    }

    /// Makes sure VCOM and VDDH have this panel's values: a value left by anything else must not reach the panel. For
    /// before the rails go up, as VDDH is not to change while they are.
    pub fn check_voltages(&mut self) -> Result<(), Error> {
        let vcom =
            (u16::from(self.bus.read(EPDVCOMR)? & 1) << 8) | u16::from(self.bus.read(EPDVCOM)?);
        if vcom != VCOM_CODE {
            self.write(EPDVCOM, VCOM_CODE as u8)?;
            self.update(EPDVCOMR, 1, (VCOM_CODE >> 8) as u8)?;
            if self.bus.read(EPDVCOM)? != VCOM_CODE as u8 {
                return Err(Error::PmicVerify { reg: EPDVCOM });
            }
        }
        if self.bus.read(EPDVDDH)? != VDDH_CODE {
            self.write(EPDVDDH, VDDH_CODE)?;
            if self.bus.read(EPDVDDH)? != VDDH_CODE {
                return Err(Error::PmicVerify { reg: EPDVDDH });
            }
        }
        Ok(())
    }

    /// Switches the panel logic's 3.2 V.
    pub fn logic(&mut self, on: bool) -> Result<(), Error> {
        let (clear, set) = if on { (0, LSW_OUTLS) } else { (LSW_OUTLS, 0) };
        self.update(LSW4_CNTRL, clear, set)
    }

    /// Switches the panel's high voltages.
    pub fn high_voltages(&mut self, on: bool) -> Result<(), Error> {
        let (clear, set) = if on {
            (0, EPDCNFG_EPDEN)
        } else {
            (EPDCNFG_EPDEN, 0)
        };
        self.update(EPDCNFG, clear, set)
    }

    /// Switches VCOM.
    pub fn vcom(&mut self, on: bool) -> Result<(), Error> {
        let (clear, set) = if on {
            (0, EPDCNFG_VCOMEN)
        } else {
            (EPDCNFG_VCOMEN, 0)
        };
        self.update(EPDCNFG, clear, set)
    }

    /// Whether the high voltages or VCOM are on, and whether the panel's logic is.
    pub fn rails_on(&mut self) -> Result<(bool, bool), Error> {
        let high = self.bus.read(EPDCNFG)? & (EPDCNFG_EPDEN | EPDCNFG_VCOMEN) != 0;
        let logic = self.bus.read(LSW4_CNTRL)? & LSW_OUTLS != 0;
        Ok((high, logic))
    }

    /// Any fault the rails report now.
    pub fn faults(&mut self) -> Result<Option<u8>, Error> {
        let faults = self.bus.read(EPDINTS)? & EPDINTS_FAULTS;
        Ok((faults != 0).then_some(faults))
    }

    /// Whether the power key is held now.
    pub fn power_key_held(&mut self) -> Result<bool, Error> {
        Ok(self.bus.read(GLBLSTAT)? & GLBLSTAT_EN0 != 0)
    }

    /// The frontlight's level, 0 when it is off.
    pub fn brightness(&mut self) -> Result<u16, Error> {
        if self.bus.read(LEDBST_CNTRL1)? & LED1EN == 0 {
            return Ok(0);
        }
        let high = self.bus.read(LED1CURRENT_1)?;
        let low = self.bus.read(LED1CURRENT_2)? & 0xf;
        Ok((u16::from(high) << 4) | u16::from(low))
    }

    /// Sets the frontlight's level. Off keeps the level in its registers, as under Linux.
    pub fn set_brightness(&mut self, level: u16) -> Result<(), Error> {
        let level = level.min(MAX_BRIGHTNESS);
        if level == 0 {
            return self.update(LEDBST_CNTRL1, LED1EN, 0);
        }
        self.write(LED1CURRENT_1, (level >> 4) as u8)?;
        self.write(LED1CURRENT_2, (level & 0xf) as u8)?;
        self.update(LEDBST_CNTRL1, 0, LED1EN)
    }

    fn adc_read(&mut self, (channel, mux): (u8, u8)) -> Result<u16, Error> {
        self.write(ADCCNTL, ADCCNTL_EN | ADCCNTL_AVG32)?;
        let (sel0, sel1) = if channel < 8 {
            (1 << channel, 0)
        } else {
            (0, 1 << (channel - 8))
        };
        self.write(ADCSEL0, sel0)?;
        self.write(ADCSEL1, sel1)?;
        self.write(ADCICNFG, (mux << 2) | ADCICNFG_50UA)?;
        self.write(ADCCNTL, ADCCNTL_EN | ADCCNTL_AVG32 | ADCCNTL_CONV)?;
        let mut done = false;
        for _ in 0..10 {
            if self.bus.read(ADCCNTL)? & ADCCNTL_CONV == 0 {
                done = true;
                break;
            }
            self.bus.delay_us(1000);
        }
        let result = if done {
            self.write(ADCCHSEL, channel)?;
            let high = self.bus.read(ADCDATAH)?;
            let low = self.bus.read(ADCDATAL)?;
            Ok((u16::from(high) << 8) | u16::from(low))
        } else {
            Err(Error::AdcTimeout)
        };
        self.write(ADCSEL0, 0)?;
        self.write(ADCSEL1, 0)?;
        self.write(ADCCNTL, 0)?;
        result
    }

    /// The panel's temperature: the thermistor against the ground of its analog island.
    pub fn temperature(&mut self) -> Result<i32, Error> {
        self.write(ADCDLY, 0)?;
        let ain0 = self.adc_read(AIN0)?;
        let ain2 = self.adc_read(AIN2)?;
        Ok(ntc_temp(ain0.abs_diff(ain2)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::FakePmic;

    #[test]
    fn nothing_outside_the_list_can_be_written() {
        let mut pmic = Pmic::new(FakePmic::default());
        for reg in 0..=255u8 {
            let val = match reg {
                EPDVDDH => VDDH_CODE,
                EPDVCOM => VCOM_CODE as u8,
                _ => 0,
            };
            let result = pmic.write(reg, val);
            assert_eq!(result.is_ok(), WRITABLE.contains(&reg), "register {reg:#x}");
        }
    }

    #[test]
    fn the_list_has_no_global_charger_or_other_panel_voltage_register() {
        // GLBLCNFG0, the bucks' voltages, the charger, the touch's switches, and the panel's other voltages and sequence.
        for reg in [0x00, 0x31, 0x38, 0x5d, 0x5e, 0x65, 0x66, 0x67, 0x69, 0xb5] {
            assert!(!WRITABLE.contains(&reg), "register {reg:#x}");
        }
    }

    #[test]
    fn only_this_panels_voltages_can_be_written() {
        let mut pmic = Pmic::new(FakePmic::default());
        assert!(pmic.write(EPDVDDH, VDDH_CODE).is_ok());
        assert_eq!(
            pmic.write(EPDVDDH, VDDH_CODE + 1),
            Err(Error::PmicRefused {
                reg: EPDVDDH,
                val: VDDH_CODE + 1
            })
        );
        assert!(pmic.write(EPDVCOM, 244).is_ok());
        assert!(pmic.write(EPDVCOM, 245).is_err());
        assert!(pmic.write(EPDVCOMR, 0).is_ok());
        assert!(pmic.write(EPDVCOMR, 1).is_err());
        assert!(pmic.write(EPDVCOMR, 2).is_ok());
    }

    #[test]
    fn right_voltages_are_left_alone() {
        let mut bus = FakePmic::default();
        bus.set(EPDVCOM, 244);
        bus.set(EPDVDDH, 21);
        let log = bus.writes();
        Pmic::new(bus).check_voltages().unwrap();
        assert!(log.borrow().is_empty());
    }

    #[test]
    fn wrong_voltages_are_set_right_and_read_back() {
        let mut bus = FakePmic::default();
        bus.set(EPDVCOM, 100);
        bus.set(EPDVCOMR, 1);
        bus.set(EPDVDDH, 15);
        let log = bus.writes();
        let probe = bus.clone();
        Pmic::new(bus).check_voltages().unwrap();
        assert_eq!(probe.get(EPDVCOM), 244);
        assert_eq!(probe.get(EPDVCOMR), 0);
        assert_eq!(probe.get(EPDVDDH), 21);
        assert_eq!(log.borrow().len(), 3);
    }

    #[test]
    fn switching_a_rail_keeps_the_registers_other_bits() {
        let mut bus = FakePmic::default();
        bus.set(LSW4_CNTRL, 0x0c);
        bus.set(EPDCNFG, 0x3e);
        let probe = bus.clone();
        let mut pmic = Pmic::new(bus);
        pmic.logic(true).unwrap();
        assert_eq!(probe.get(LSW4_CNTRL), 0x0d);
        pmic.logic(false).unwrap();
        assert_eq!(probe.get(LSW4_CNTRL), 0x0c);
        pmic.high_voltages(true).unwrap();
        pmic.vcom(true).unwrap();
        assert_eq!(probe.get(EPDCNFG), 0xfe);
        pmic.vcom(false).unwrap();
        pmic.high_voltages(false).unwrap();
        assert_eq!(probe.get(EPDCNFG), 0x3e);
    }

    #[test]
    fn the_frontlight_keeps_the_other_bits_and_its_level_when_off() {
        let mut bus = FakePmic::default();
        bus.set(LEDBST_CNTRL1, 0x14);
        let probe = bus.clone();
        let mut pmic = Pmic::new(bus);
        assert_eq!(pmic.brightness(), Ok(0));
        pmic.set_brightness(0x123).unwrap();
        assert_eq!(
            (probe.get(LED1CURRENT_1), probe.get(LED1CURRENT_2)),
            (0x12, 0x3)
        );
        assert_eq!(probe.get(LEDBST_CNTRL1), 0x94);
        assert_eq!(pmic.brightness(), Ok(0x123));
        pmic.set_brightness(0).unwrap();
        assert_eq!(probe.get(LEDBST_CNTRL1), 0x14);
        assert_eq!(probe.get(LED1CURRENT_1), 0x12);
        assert_eq!(pmic.brightness(), Ok(0));
        pmic.set_brightness(u16::MAX).unwrap();
        assert_eq!(pmic.brightness(), Ok(MAX_BRIGHTNESS));
    }

    #[test]
    fn the_power_key_is_bit_7_of_its_status() {
        let mut bus = FakePmic::default();
        let mut pmic = Pmic::new(bus.clone());
        assert_eq!(pmic.power_key_held(), Ok(false));
        bus.set(GLBLSTAT, 0x80);
        assert_eq!(pmic.power_key_held(), Ok(true));
    }

    #[test]
    fn a_fault_bit_is_reported_and_the_vcom_bit_is_not_one() {
        let mut bus = FakePmic::default();
        let probe = bus.clone();
        let mut pmic = Pmic::new(bus.clone());
        assert_eq!(pmic.faults(), Ok(None));
        bus.set(EPDINTS, 0x40);
        assert_eq!(pmic.faults(), Ok(None));
        bus.set(EPDINTS, 0x04);
        assert_eq!(pmic.faults(), Ok(Some(0x04)));
        let _ = probe;
    }

    #[test]
    fn thermistor_codes_give_the_colder_neighbour_and_clamp() {
        assert_eq!(ntc_temp(0), 75);
        assert_eq!(ntc_temp(0x095), 75);
        assert_eq!(ntc_temp(0x096), 74);
        assert_eq!(ntc_temp(0x710), -10);
        assert_eq!(ntc_temp(0xfff), -10);
        // 25 C is entry 50.
        assert_eq!(ntc_temp(NTC_CODES[50]), 25);
    }

    #[test]
    fn a_temperature_is_the_difference_of_two_channels_and_leaves_the_adc_off() {
        let mut bus = FakePmic::default();
        bus.adc_codes(0x2a0, 0x00b);
        let probe = bus.clone();
        let temp = Pmic::new(bus).temperature().unwrap();
        assert_eq!(temp, ntc_temp(0x295));
        assert_eq!(probe.get(ADCCNTL), 0);
        assert_eq!(probe.get(ADCSEL0), 0);
        assert_eq!(probe.get(ADCSEL1), 0);
    }

    #[test]
    fn a_stuck_adc_is_an_error_and_is_still_switched_off() {
        let mut bus = FakePmic::default();
        bus.stuck_adc();
        let probe = bus.clone();
        assert_eq!(Pmic::new(bus).temperature(), Err(Error::AdcTimeout));
        assert_eq!(probe.get(ADCCNTL), 0);
    }
}
