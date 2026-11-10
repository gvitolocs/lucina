//! MAX77696 PMIC (I2C1, 0x3c): only the EPD panel supplies for Phase 0.
//!
//! Sequence and registers from our Linux drivers (`drivers/regulator/max77696-epd.c`, `max77696-regulator.c`,
//! `include/linux/mfd/max77696.h`) and the PW3 device tree (`imx6sl-amazon-wario.dts`): LSW4 is the panel
//! logic 3.2 V ("V3P3"), EPDCNFG.EPDEN the panel high voltages ("DISPLAY"), EPDCNFG.VCOMEN the VCOM rail.
//! Power good is the EPDC_PWRSTAT pad, GPIO2_IO13. Nothing else on the PMIC is touched.

use super::i2c::{I2c, Result};
use crate::arch::{timer, Mmio};

pub const ADDR: u8 = 0x3c;

const REG_LSW4_CNTRL: u8 = 0x5c + 3;
const LSW_OUTLS: u8 = 1 << 0;
const REG_EPDCNFG: u8 = 0x60;
const EPDCNFG_EPDEN: u8 = 1 << 7;
const EPDCNFG_VCOMEN: u8 = 1 << 6;
const REG_EPDINTS: u8 = 0x61;
const REG_EPDVCOM: u8 = 0x64;
const REG_EPDVCOMR: u8 = 0xb4;

/// Panel ED060KD1C2 VCOM -2.44 V = code 244 (9.985 mV steps), from the PW3 device tree.
const VCOM_CODE: u16 = 244;

const GPIO2: Mmio = Mmio::at(0x020a_0000);
const GPIO_PSR: usize = 0x08;
const PWRSTAT_BIT: u32 = 1 << 13;

pub struct Pmic<'a> {
    pub bus: &'a I2c,
}

pub struct EpdStatus {
    pub vcom_code: u16,
    pub vcom_written: bool,
    pub power_good: bool,
    pub faults: u8,
}

impl Pmic<'_> {
    fn rd(&self, r: u8) -> Result<u8> {
        self.bus.read_reg(ADDR, r)
    }

    pub fn epdcnfg(&self) -> Result<u8> {
        self.rd(REG_EPDCNFG)
    }

    fn vcom(&self) -> Result<u16> {
        Ok(((self.rd(REG_EPDVCOMR)? as u16 & 1) << 8) | self.rd(REG_EPDVCOM)? as u16)
    }

    fn power_good() -> bool {
        GPIO2.read(GPIO_PSR) & PWRSTAT_BIT != 0
    }

    /// LSW4, then EPDEN (wait for power good, 250 ms like Linux), then VCOM.
    pub fn epd_on(&self) -> Result<EpdStatus> {
        let mut st = EpdStatus { vcom_code: self.vcom()?, vcom_written: false, power_good: false, faults: 0 };
        if st.vcom_code != VCOM_CODE {
            self.bus.write_reg(ADDR, REG_EPDVCOM, (VCOM_CODE & 0xff) as u8)?;
            self.bus.update_reg(ADDR, REG_EPDVCOMR, 1, (VCOM_CODE >> 8) as u8)?;
            st.vcom_written = true;
            st.vcom_code = self.vcom()?;
        }
        self.bus.update_reg(ADDR, REG_LSW4_CNTRL, 0, LSW_OUTLS)?;
        timer::delay_ms(2);
        self.bus.update_reg(ADDR, REG_EPDCNFG, 0, EPDCNFG_EPDEN)?;
        st.power_good = timer::poll_until(250, Self::power_good);
        st.faults = self.rd(REG_EPDINTS)?;
        self.bus.update_reg(ADDR, REG_EPDCNFG, 0, EPDCNFG_VCOMEN)?;
        Ok(st)
    }

    /// Reverse order: VCOM, EPDEN (wait up to 10 ms for power good to drop), LSW4.
    pub fn epd_off(&self) -> Result<()> {
        self.bus.update_reg(ADDR, REG_EPDCNFG, EPDCNFG_VCOMEN, 0)?;
        self.bus.update_reg(ADDR, REG_EPDCNFG, EPDCNFG_EPDEN, 0)?;
        let _ = timer::poll_until(10, || !Self::power_good());
        self.bus.update_reg(ADDR, REG_LSW4_CNTRL, LSW_OUTLS, 0)?;
        Ok(())
    }
}
