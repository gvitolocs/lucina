//! Powers the i.MX6SL's USB OTG1 port on the Kindle Paperwhite 3.
//!
//! The order is that of Linux on this board: the clocks, the forced session, the non-core settings, the PHY's reset,
//! its 3.0 V from the PMIC, then the PHY itself. After a kexec from Linux the controller, the PHY and LDO2 are all off,
//! as its shutdown path turns them off, and the controller's clock is gated: any register access before
//! [`bring_up`] ungates it hangs the bus.

use crate::{Error, Hw};

const CCM: usize = 0x020c_4000;
const CCM_CCGR2: usize = CCM + 0x70;
const CCM_CCGR6: usize = CCM + 0x80;
/// CCGR2: the I2C1 clock.
const CCGR2_I2C1: u32 = 3 << 6;
/// CCGR6: the clock of the USB controllers and their non-core registers.
const CCGR6_USBOH3: u32 = 3;

const ANATOP: usize = 0x020c_8000;
// The registers here have aliases that set and clear bits at +4 and +8.
const PLL3: usize = ANATOP + 0x10;
const PLL3_SET: usize = PLL3 + 4;
const PLL3_CLR: usize = PLL3 + 8;
const PLL3_EN_USB_CLKS: u32 = 1 << 6;
const PLL3_POWER: u32 = 1 << 12;
const PLL3_ENABLE: u32 = 1 << 13;
const PLL3_BYPASS: u32 = 1 << 16;
const PLL3_LOCK: u32 = 1 << 31;
/// VBUS reaches only the PMIC on this board, so the SoC's session is forced.
const USB1_VBUS_DETECT_SET: usize = ANATOP + 0x1a4;
const USB1_VBUS_DETECT_CLR: usize = ANATOP + 0x1a8;
const VBUS_OVERRIDE_EN: u32 = 1 << 3;
const SESSEND_OVERRIDE: u32 = 1 << 4;
const BVALID_OVERRIDE: u32 = 1 << 5;
const VBUSVALID_OVERRIDE: u32 = 1 << 7;
/// The charger detectors load the data lines ("the signal at DP will be poor"). Linux's port number mapping writes
/// the other port's register for this one; both go off here.
const USB1_CHRG_DETECT_SET: usize = ANATOP + 0x1b4;
const USB2_CHRG_DETECT_SET: usize = ANATOP + 0x214;
const CHRG_DETECT_OFF: u32 = (1 << 20) | (1 << 19);

/// The controller's port register: PHCD stops the PHY's clock, and Linux's shutdown leaves it set. While it is set the
/// PHY's registers do not respond at all: reads give stale bus data and writes are lost, so it must clear first.
const USB_PORTSC: usize = 0x0218_4184;
const PORTSC_PHCD: u32 = 1 << 23;
/// PORTSC's write-1-to-clear bits, which a read-modify-write must not send back.
const PORTSC_W1C: u32 = (1 << 1) | (1 << 3) | (1 << 5);

const USBMISC_OTG1_CTRL: usize = 0x0218_4800;
const OTG1_NON_BURST: u32 = 1 << 1;
const OTG1_OVER_CUR_DIS: u32 = 1 << 7;
const OTG1_WAKEUPS: u32 = (1 << 10) | (1 << 16) | (1 << 17) | (1 << 29);

const PHY: usize = 0x020c_9000;
const PHY_PWD: usize = PHY;
const PHY_CTRL: usize = PHY + 0x30;
const PHY_CTRL_SET: usize = PHY + 0x34;
const PHY_CTRL_CLR: usize = PHY + 0x38;
const PHY_IP_SET: usize = PHY + 0x94;
const CTRL_SFTRST: u32 = 1 << 31;
const CTRL_CLKGATE: u32 = 1 << 30;
/// Automatic clocks and power, and full and low speed support.
const CTRL_RUN: u32 =
    (1 << 26) | (1 << 25) | (1 << 20) | (1 << 19) | (1 << 18) | (1 << 15) | (1 << 14);
const IP_FIX: u32 = (1 << 17) | (1 << 18);

const I2C1: usize = 0x021a_0000;
const I2C_IFDR: usize = I2C1 + 0x04;
const I2C_I2CR: usize = I2C1 + 0x08;
const I2C_I2SR: usize = I2C1 + 0x0c;
const I2C_I2DR: usize = I2C1 + 0x10;
const I2CR_IEN: u16 = 1 << 7;
const I2CR_MSTA: u16 = 1 << 5;
const I2CR_MTX: u16 = 1 << 4;
const I2CR_TXAK: u16 = 1 << 3;
const I2CR_RSTA: u16 = 1 << 2;
const I2SR_IBB: u16 = 1 << 5;
const I2SR_IAL: u16 = 1 << 4;
const I2SR_IIF: u16 = 1 << 1;
const I2SR_RXAK: u16 = 1 << 0;
/// Dividing by 192: under 400 kHz from any clock this block has, for a kernel that set no rate.
const IFDR_FALLBACK: u16 = 0x31;

/// The MAX77696's PMIC page on I2C1.
const PMIC: u8 = 0x3c;
/// LDO2's configuration: mode in bits 7:6, voltage in bits 5:0. This is the only PMIC register this crate writes. The
/// charger and global registers must keep their values: a wrong write there overcharges or powers the board off.
const LDO2_CNFG1: u8 = 0x45;
const LDO_MODE_MASK: u8 = 0xc0;
const LDO_MODE_NORMAL: u8 = 0xc0;
/// 3.0 V: 0.8 V + 44 * 50 mV.
const LDO_TV_3V0: u8 = 0x2c;
const LDO_TV_MASK: u8 = 0x3f;

/// A generous limit for a polled step, in units of [`POLL_STEP_US`].
const POLL_LIMIT: u32 = 1000;
const POLL_STEP_US: u32 = 10;

/// Polls until `done`, within about 10 ms.
fn poll<H: Hw>(hw: &mut H, mut done: impl FnMut(&mut H) -> bool) -> bool {
    for _ in 0..POLL_LIMIT {
        if done(hw) {
            return true;
        }
        hw.delay_us(POLL_STEP_US);
    }
    done(hw)
}

/// The master side of an i.MX I2C controller, polled.
struct I2c<'a, H: Hw>(&'a mut H);

impl<H: Hw> I2c<'_, H> {
    fn init(&mut self) {
        if self.0.read16(I2C_IFDR) & 0x3f == 0 {
            self.0.write16(I2C_IFDR, IFDR_FALLBACK);
        }
        self.0.write16(I2C_I2CR, 0);
        self.0.write16(I2C_I2SR, 0);
        self.0.write16(I2C_I2CR, I2CR_IEN);
        self.0.delay_us(50);
    }

    fn wait_transfer(&mut self) -> Result<(), Error> {
        if !poll(self.0, |hw| hw.read16(I2C_I2SR) & I2SR_IIF != 0) {
            return Err(Error::I2cTimeout);
        }
        let status = self.0.read16(I2C_I2SR);
        self.0.write16(I2C_I2SR, 0);
        if status & I2SR_IAL != 0 {
            return Err(Error::I2cTimeout);
        }
        Ok(())
    }

    fn send(&mut self, byte: u8) -> Result<(), Error> {
        self.0.write16(I2C_I2DR, byte.into());
        self.wait_transfer()?;
        if self.0.read16(I2C_I2SR) & I2SR_RXAK != 0 {
            return Err(Error::I2cNak);
        }
        Ok(())
    }

    fn start(&mut self) -> Result<(), Error> {
        if !poll(self.0, |hw| hw.read16(I2C_I2SR) & I2SR_IBB == 0) {
            return Err(Error::I2cTimeout);
        }
        self.0.write16(I2C_I2CR, I2CR_IEN | I2CR_MSTA | I2CR_MTX);
        if !poll(self.0, |hw| hw.read16(I2C_I2SR) & I2SR_IBB != 0) {
            return Err(Error::I2cTimeout);
        }
        Ok(())
    }

    fn stop(&mut self) {
        self.0.write16(I2C_I2CR, I2CR_IEN);
        let _ = poll(self.0, |hw| hw.read16(I2C_I2SR) & I2SR_IBB == 0);
    }

    /// Ends a transaction, resetting the controller after a failure for the next one.
    fn finish<T>(&mut self, result: Result<T, Error>) -> Result<T, Error> {
        self.stop();
        if result.is_err() {
            self.0.write16(I2C_I2CR, 0);
            self.0.delay_us(50);
            self.0.write16(I2C_I2CR, I2CR_IEN);
        }
        result
    }

    fn write_reg(&mut self, addr: u8, reg: u8, val: u8) -> Result<(), Error> {
        let result = self.start().and_then(|()| {
            self.send(addr << 1)?;
            self.send(reg)?;
            self.send(val)
        });
        self.finish(result)
    }

    fn read_reg(&mut self, addr: u8, reg: u8) -> Result<u8, Error> {
        let result = self.start().and_then(|()| {
            self.send(addr << 1)?;
            self.send(reg)?;
            self.0
                .write16(I2C_I2CR, I2CR_IEN | I2CR_MSTA | I2CR_MTX | I2CR_RSTA);
            self.send((addr << 1) | 1)?;
            // One byte: receive with a NAK, and stop before reading the data register.
            self.0.write16(I2C_I2CR, I2CR_IEN | I2CR_MSTA | I2CR_TXAK);
            let _ = self.0.read16(I2C_I2DR);
            self.wait_transfer()?;
            self.0.write16(I2C_I2CR, I2CR_IEN | I2CR_TXAK);
            let _ = poll(self.0, |hw| hw.read16(I2C_I2SR) & I2SR_IBB == 0);
            Ok(self.0.read16(I2C_I2DR) as u8)
        });
        self.finish(result)
    }
}

/// Turns on LDO2 at 3.0 V, keeping its value if it is right already.
fn enable_ldo2<H: Hw>(hw: &mut H) -> Result<(), Error> {
    let mut i2c = I2c(hw);
    let old = i2c.read_reg(PMIC, LDO2_CNFG1)?;
    let new = (old & !(LDO_MODE_MASK | LDO_TV_MASK)) | LDO_MODE_NORMAL | LDO_TV_3V0;
    if new != old {
        i2c.write_reg(PMIC, LDO2_CNFG1, new)?;
    }
    if i2c.read_reg(PMIC, LDO2_CNFG1)? != new {
        return Err(Error::I2cNak);
    }
    // Neither Linux nor Amazon waits here, relying on the time their next steps take. The rail has a finite ramp.
    hw.delay_us(1000);
    Ok(())
}

/// Brings up the PLL that clocks the port, which Linux leaves running for other blocks too.
fn enable_pll3<H: Hw>(hw: &mut H) -> Result<(), Error> {
    hw.write32(PLL3_CLR, PLL3_BYPASS);
    hw.write32(PLL3_SET, PLL3_POWER);
    if !poll(hw, |hw| hw.read32(PLL3) & PLL3_LOCK != 0) {
        return Err(Error::PllNotLocked);
    }
    hw.write32(PLL3_SET, PLL3_ENABLE | PLL3_EN_USB_CLKS);
    // "PHY needs some 32K cycles to switch from 32K clock to bus clock".
    hw.delay_us(400);
    Ok(())
}

/// Resets the PHY block, as Amazon's kernel does: the reset, a pause, then neither the reset nor the clock gate. Linux's
/// generic block reset also waits for the gate to set itself, which this PHY did not do after a kexec.
fn reset_phy<H: Hw>(hw: &mut H) -> Result<(), Error> {
    hw.write32(PHY_CTRL_SET, CTRL_SFTRST);
    hw.delay_us(10);
    hw.write32(PHY_CTRL_CLR, CTRL_SFTRST | CTRL_CLKGATE);
    hw.delay_us(10);
    if !poll(hw, |hw| {
        hw.read32(PHY_CTRL) & (CTRL_SFTRST | CTRL_CLKGATE) == 0
    }) {
        return Err(Error::PhyReset {
            ctrl: hw.read32(PHY_CTRL),
        });
    }
    Ok(())
}

/// The registers that say how far the port got, for the log when [`bring_up`] fails.
pub fn registers<H: Hw>(hw: &mut H) -> [(&'static str, u32); 8] {
    [
        ("CCGR2", hw.read32(CCM_CCGR2)),
        ("CCGR6", hw.read32(CCM_CCGR6)),
        ("PLL3", hw.read32(PLL3)),
        ("VBUS_DETECT", hw.read32(ANATOP + 0x1a0)),
        ("PHY_PWD", hw.read32(PHY_PWD)),
        ("PHY_CTRL", hw.read32(PHY_CTRL)),
        ("PHY_STATUS", hw.read32(PHY + 0x40)),
        ("PHY_DEBUG", hw.read32(PHY + 0x50)),
    ]
}

/// Powers the port and its PHY, so that [`crate::dcd::Dcd::start`] can run.
/// Whether the power key is down, read from the PMIC. For what runs before any driver: it sets up what it needs.
pub fn power_key_held<H: Hw>(hw: &mut H) -> Result<bool, Error> {
    const GLBLSTAT: u8 = 0x07;
    const GLBLSTAT_EN0: u8 = 1 << 7;
    let gate = hw.read32(CCM_CCGR2);
    hw.write32(CCM_CCGR2, gate | CCGR2_I2C1);
    let mut i2c = I2c(hw);
    i2c.init();
    Ok(i2c.read_reg(PMIC, GLBLSTAT)? & GLBLSTAT_EN0 != 0)
}

/// One register of the PMIC's I2C devices (0x3c the power manager, 0x35 its USB interface), read, for a boot loader that
/// sets the board up as U-Boot did. The caller has the I2C1 pads and clock up.
pub fn pmic_read<H: Hw>(hw: &mut H, device: u8, reg: u8) -> Result<u8, Error> {
    let mut i2c = I2c(hw);
    i2c.init();
    i2c.read_reg(device, reg)
}

/// As [`pmic_read`], written.
pub fn pmic_write<H: Hw>(hw: &mut H, device: u8, reg: u8, value: u8) -> Result<(), Error> {
    let mut i2c = I2c(hw);
    i2c.init();
    i2c.write_reg(device, reg, value)
}

pub fn bring_up<H: Hw>(hw: &mut H) -> Result<(), Error> {
    let gate = hw.read32(CCM_CCGR2);
    hw.write32(CCM_CCGR2, gate | CCGR2_I2C1);
    I2c(hw).init();

    let gate = hw.read32(CCM_CCGR6);
    hw.write32(CCM_CCGR6, gate | CCGR6_USBOH3);
    enable_pll3(hw)?;

    hw.write32(USB1_VBUS_DETECT_CLR, SESSEND_OVERRIDE);
    hw.write32(
        USB1_VBUS_DETECT_SET,
        VBUS_OVERRIDE_EN | BVALID_OVERRIDE | VBUSVALID_OVERRIDE,
    );

    let misc = hw.read32(USBMISC_OTG1_CTRL);
    hw.write32(
        USBMISC_OTG1_CTRL,
        (misc | OTG1_OVER_CUR_DIS | OTG1_NON_BURST) & !OTG1_WAKEUPS,
    );

    let portsc = hw.read32(USB_PORTSC);
    if portsc & PORTSC_PHCD != 0 {
        hw.write32(USB_PORTSC, portsc & !PORTSC_PHCD & !PORTSC_W1C);
        // As Amazon's kernel, before it trusts the PHY.
        hw.delay_us(1000);
    }

    reset_phy(hw)?;
    enable_ldo2(hw)?;
    hw.write32(PHY_PWD, 0);
    hw.write32(PHY_CTRL_SET, CTRL_RUN);
    hw.write32(PHY_IP_SET, IP_FIX);
    hw.write32(USB1_CHRG_DETECT_SET, CHRG_DETECT_OFF);
    hw.write32(USB2_CHRG_DETECT_SET, CHRG_DETECT_OFF);
    // "OTGSC status inputs are debounced using a 1 ms time constant": Linux waits 2 ms here.
    hw.delay_us(3000);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Access, Model};

    #[test]
    fn the_order_is_clocks_session_noncore_phy_reset_ldo2_phy() {
        let mut model = Model::new();
        bring_up(&mut model).unwrap();
        let writes = model.writes32();
        let index = |addr: usize, val: u32| {
            writes
                .iter()
                .position(|w| *w == (addr, val))
                .unwrap_or_else(|| panic!("no write of {val:#x} to {addr:#x}"))
        };
        let usb_clock = index(CCM_CCGR6, CCGR6_USBOH3);
        let pll = index(PLL3_SET, PLL3_ENABLE | PLL3_EN_USB_CLKS);
        let session = index(USB1_VBUS_DETECT_SET, 0xa8);
        let phy_on = index(PHY_PWD, 0);
        let run = index(PHY_CTRL_SET, 0x061c_c000);
        assert!(usb_clock < pll && pll < session && session < phy_on && phy_on < run);
        assert!(index(USB1_VBUS_DETECT_CLR, 0x10) < session);
        assert!(run < index(PHY_IP_SET, 0x0006_0000));
        assert!(run < index(USB1_CHRG_DETECT_SET, 0x0018_0000));
        // The non-core block needs the clock.
        let misc = writes
            .iter()
            .position(|w| w.0 == USBMISC_OTG1_CTRL)
            .unwrap();
        assert!(usb_clock < misc && misc < phy_on);
        // LDO2 comes on after the PHY's reset and before it powers up, as Linux does.
        let ldo = model.access_index(Access::PmicWrite(LDO2_CNFG1, 0xec));
        assert!(
            model.access_index(Access::Write32(PHY_CTRL_CLR, CTRL_SFTRST | CTRL_CLKGATE)) < ldo
        );
        assert!(ldo < model.access_index(Access::Write32(PHY_PWD, 0)));
    }

    #[test]
    fn the_phys_clock_runs_before_its_registers_are_touched_as_after_a_kexec() {
        let mut model = Model::new();
        model.set(USB_PORTSC, PORTSC_PHCD | 0x1000_0000 | (1 << 1));
        model.check_gates = true;
        bring_up(&mut model).unwrap();
        // The model keeps what was written, so this says that no 1 went to the write-1-to-clear bit.
        assert_eq!(model.get(USB_PORTSC), 0x1000_0000);
        assert_eq!(model.get(PHY_PWD), 0);
    }

    #[test]
    fn no_register_is_touched_before_its_clock_is_on() {
        let mut model = Model::new();
        model.check_gates = true;
        bring_up(&mut model).unwrap();
    }

    #[test]
    fn the_noncore_register_gets_the_overcurrent_and_wakeup_settings() {
        let mut model = Model::new();
        model.set(USBMISC_OTG1_CTRL, !OTG1_OVER_CUR_DIS);
        bring_up(&mut model).unwrap();
        let val = model.get(USBMISC_OTG1_CTRL);
        assert_eq!(val & OTG1_OVER_CUR_DIS, OTG1_OVER_CUR_DIS);
        assert_eq!(val & OTG1_NON_BURST, OTG1_NON_BURST);
        assert_eq!(val & OTG1_WAKEUPS, 0);
    }

    #[test]
    fn ldo2_goes_to_normal_mode_at_three_volts_and_keeps_its_other_bits_out_of_the_way() {
        let mut model = Model::new();
        model.pmic[LDO2_CNFG1 as usize] = 0x2c;
        bring_up(&mut model).unwrap();
        assert_eq!(model.pmic[LDO2_CNFG1 as usize], 0xec);
    }

    #[test]
    fn the_only_pmic_register_ever_written_is_ldo2s() {
        let mut model = Model::new();
        bring_up(&mut model).unwrap();
        assert_eq!(model.pmic_writes, [(LDO2_CNFG1, 0xec)]);
    }

    #[test]
    fn ldo2_already_right_is_not_written() {
        let mut model = Model::new();
        model.pmic[LDO2_CNFG1 as usize] = 0xec;
        bring_up(&mut model).unwrap();
        assert!(model.pmic_writes.is_empty());
    }

    #[test]
    fn a_pmic_that_does_not_answer_stops_the_bring_up_before_the_phy_powers_up() {
        let mut model = Model::new();
        model.pmic_nak = true;
        assert_eq!(bring_up(&mut model), Err(Error::I2cNak));
        assert!(!model.writes32().contains(&(PHY_PWD, 0)));
    }

    #[test]
    fn a_pll_that_does_not_lock_is_an_error() {
        let mut model = Model::new();
        model.pll_locks = false;
        assert_eq!(bring_up(&mut model), Err(Error::PllNotLocked));
    }

    #[test]
    fn a_phy_stuck_in_reset_is_an_error() {
        let mut model = Model::new();
        model.phy_stuck = true;
        assert!(matches!(bring_up(&mut model), Err(Error::PhyReset { .. })));
    }

    #[test]
    fn a_rate_left_by_the_previous_kernel_stays() {
        let mut model = Model::new();
        model.i2c_ifdr = 0x2b;
        bring_up(&mut model).unwrap();
        assert_eq!(model.i2c_ifdr, 0x2b);
        let mut model = Model::new();
        model.i2c_ifdr = 0;
        bring_up(&mut model).unwrap();
        assert_eq!(model.i2c_ifdr, IFDR_FALLBACK);
    }
}
