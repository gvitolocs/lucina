//! Powering the device off for good, and putting back what that resets.
//!
//! This is the one place outside [`crate::pmic`] that writes the PMIC, and it writes only the registers named here.
//!
//! * [`power_off`] is Linux's and Amazon's power-off: `GLBLCNFG0` (0x00) cleared, then `FSENT | SFTPDRR` (bits 5 and 0),
//!   as `max77696_power_off` in the mainline based Linux that runs on this device and `max77696_chip_poweroff` in
//!   Amazon's 3.0.35. The PMIC acts on a 0 -> 1 edge of a bit, hence the clear first: `SFTPDRR` reads as 1 already at
//!   boot. `FSENT` is factory-ship mode. Only the power key, or a charger plugged in, turns the device on again: the
//!   RTC alarm does not, seen on the device on 2026-11-03.
//! * `SFTPDRR` resets the PMIC's registers on the way down. [`restore`] puts back the ones this kernel relies on, with
//!   the values Linux's drivers write on every probe (Amazon did the same on every boot): `GLBLCNFG1.STBYEN` (bit 0),
//!   so that the SoC's standby request puts the PMIC in its low-power state in a deep sleep; `GLBLCNFG3` = LBDAC 3.2 V
//!   and LBHYST 200 mV, below which the PMIC shuts down by itself; and the charger's constant voltage, 4.2 V
//!   (`CHG_CNFG_04.CHG_CV_PRM` code 22, behind `CHG_CNFG_06.CHGPROT`), which comes back as 4.0 V after a reset.
//!
//! Nothing else on the PMIC is written: never `GLBLCNFG1.GLBL_LPM` (bit 6, it hung the Kindle in a sleep), and of the
//! charger only the constant voltage and its protection, never its watchdog.

use crate::{Error, pmic::PmicBus};

/// The PMIC's power actions, each taken on a 0 -> 1 edge.
const GLBLCNFG0: u8 = 0x00;
const GLBLCNFG0_FSENT: u8 = 1 << 5;
const GLBLCNFG0_SFTPDRR: u8 = 1 << 0;
const GLBLCNFG1: u8 = 0x01;
const GLBLCNFG1_GLBL_LPM: u8 = 1 << 6;
const GLBLCNFG1_STBYEN: u8 = 1 << 0;
const GLBLCNFG3: u8 = 0x03;
/// LBHYST (bits 4:3) and LBDAC (bits 2:0).
const GLBLCNFG3_LB: u8 = 0x1f;
/// LBDAC code 5 = 2.7 V + 5 * 100 mV, LBHYST code 0 = 200 mV: Linux's device tree for this board.
const GLBLCNFG3_LB_VALUE: u8 = 5;
const CHG_CNFG_04: u8 = 0x13;
const CHG_CV_PRM: u8 = 0x1f;
/// 4.2 V: the charge voltage this battery is charged to, by Amazon's kernel and Linux's.
pub const CHG_CV_CODE: u8 = 22;
const CHG_CNFG_06: u8 = 0x15;
const CHG_CNFG_06_CHGPROT: u8 = 3 << 2;

/// The constant charge voltage of a `CHG_CV_PRM` code, in mV.
pub fn cv_mv(code: u8) -> u32 {
    const STEPS: [u16; 4] = [4340, 4350, 4375, 4400];
    match code & CHG_CV_PRM {
        code @ 0..=27 => 3650 + 25 * u32::from(code),
        code @ 28..=31 => u32::from(STEPS[usize::from(code - 28)]),
        _ => unreachable!(),
    }
}

/// Powers the device off. Returns only if the PMIC did not take the writes; when they reach it the power goes within
/// milliseconds, so a caller that is still running half a second later knows it did not work.
pub fn power_off<B: PmicBus>(bus: &mut B) -> Result<(), Error> {
    bus.write(GLBLCNFG0, 0)?;
    bus.write(GLBLCNFG0, GLBLCNFG0_FSENT | GLBLCNFG0_SFTPDRR)
}

/// What [`restore`] found, for the log: each field is the value it had, where it had to be put back.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Restored {
    pub glblcnfg1: Option<u8>,
    pub glblcnfg3: Option<u8>,
    /// The charger's constant voltage code before, if it was not 4.2 V.
    pub cv_code: Option<u8>,
}

impl Restored {
    /// Whether the PMIC had been reset since a kernel last set it up.
    pub fn any(&self) -> bool {
        self.glblcnfg1.is_some() || self.glblcnfg3.is_some() || self.cv_code.is_some()
    }
}

/// Puts back what a power-off with `SFTPDRR` resets and this kernel relies on (see the module's documentation). Leaves
/// alone what already has its value, so on every boot but the first after a power-off it only reads.
pub fn restore<B: PmicBus>(bus: &mut B) -> Result<Restored, Error> {
    let mut found = Restored::default();
    let cnfg1 = bus.read(GLBLCNFG1)?;
    if cnfg1 & GLBLCNFG1_STBYEN == 0 {
        // Writing the value back would keep a set GLBL_LPM, and that hung the device: such a value is left alone.
        if cnfg1 & GLBLCNFG1_GLBL_LPM != 0 {
            return Err(Error::PmicRefused {
                reg: GLBLCNFG1,
                val: cnfg1 | GLBLCNFG1_STBYEN,
            });
        }
        bus.write(GLBLCNFG1, cnfg1 | GLBLCNFG1_STBYEN)?;
        found.glblcnfg1 = Some(cnfg1);
    }
    let cnfg3 = bus.read(GLBLCNFG3)?;
    if cnfg3 & GLBLCNFG3_LB != GLBLCNFG3_LB_VALUE {
        bus.write(GLBLCNFG3, (cnfg3 & !GLBLCNFG3_LB) | GLBLCNFG3_LB_VALUE)?;
        found.glblcnfg3 = Some(cnfg3);
    }
    let cv = bus.read(CHG_CNFG_04)?;
    if cv & CHG_CV_PRM != CHG_CV_CODE {
        let prot = bus.read(CHG_CNFG_06)?;
        bus.write(CHG_CNFG_06, prot | CHG_CNFG_06_CHGPROT)?;
        let written = bus.write(CHG_CNFG_04, (cv & !CHG_CV_PRM) | CHG_CV_CODE);
        // Closed again even if the write failed.
        let closed = bus.write(CHG_CNFG_06, prot & !CHG_CNFG_06_CHGPROT);
        written.and(closed)?;
        if bus.read(CHG_CNFG_04)? & CHG_CV_PRM != CHG_CV_CODE {
            return Err(Error::PmicVerify { reg: CHG_CNFG_04 });
        }
        found.cv_code = Some(cv & CHG_CV_PRM);
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::FakePmic;

    /// Registers as a reset leaves them: standby off, no comparator, 4.0 V.
    fn after_reset() -> FakePmic {
        let mut pmic = FakePmic::default();
        pmic.set(GLBLCNFG0, 0x01);
        pmic.set(GLBLCNFG1, 0x3e);
        pmic.set(GLBLCNFG3, 0x00);
        pmic.set(CHG_CNFG_04, 0x0e);
        pmic.set(CHG_CNFG_06, 0x00);
        pmic
    }

    #[test]
    fn power_off_is_an_edge_of_fsent_and_sftpdrr() {
        let mut pmic = after_reset();
        power_off(&mut pmic).unwrap();
        assert_eq!(*pmic.writes().borrow(), [(0x00, 0x00), (0x00, 0x21)]);
    }

    #[test]
    fn restore_puts_back_standby_the_comparator_and_4_2_v() {
        let mut pmic = after_reset();
        let found = restore(&mut pmic).unwrap();
        assert_eq!(
            found,
            Restored {
                glblcnfg1: Some(0x3e),
                glblcnfg3: Some(0x00),
                cv_code: Some(14)
            }
        );
        assert_eq!(pmic.get(GLBLCNFG1), 0x3f);
        assert_eq!(pmic.get(GLBLCNFG3), 0x05);
        assert_eq!(pmic.get(CHG_CNFG_04), 0x16);
        assert_eq!(cv_mv(pmic.get(CHG_CNFG_04)), 4200);
        // The protection is open around the one protected write, and closed after it.
        assert_eq!(
            *pmic.writes().borrow(),
            [
                (0x01, 0x3f),
                (0x03, 0x05),
                (0x15, 0x0c),
                (0x13, 0x16),
                (0x15, 0x00)
            ]
        );
    }

    #[test]
    fn a_pmic_that_is_set_up_is_only_read() {
        let mut pmic = after_reset();
        pmic.set(GLBLCNFG1, 0x3f);
        pmic.set(GLBLCNFG3, 0x05);
        pmic.set(CHG_CNFG_04, 0x16);
        let found = restore(&mut pmic).unwrap();
        assert!(!found.any());
        assert!(pmic.writes().borrow().is_empty());
    }

    #[test]
    fn global_low_power_is_never_written() {
        let mut pmic = after_reset();
        pmic.set(GLBLCNFG1, GLBLCNFG1_GLBL_LPM);
        assert!(restore(&mut pmic).is_err());
        assert!(pmic.writes().borrow().is_empty());
    }

    #[test]
    fn charge_voltages_of_the_codes() {
        assert_eq!(cv_mv(0), 3650);
        assert_eq!(cv_mv(14), 4000);
        assert_eq!(cv_mv(22), 4200);
        assert_eq!(cv_mv(27), 4325);
        assert_eq!(cv_mv(28), 4340);
        assert_eq!(cv_mv(31), 4400);
    }
}
