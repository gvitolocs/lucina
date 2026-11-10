//! i.MX I2C master, polled (I2C1 at 0x021A_0000 carries the MAX77696 on the PW3).
//!
//! Register reference: Linux `drivers/i2c/busses/i2c-imx.c` (16-bit registers, 4-byte stride). The pads and the
//! bus rate (IFDR) are left as the previous kernel set them.

use crate::arch::{timer, Mmio};

pub struct I2c(Mmio);

const IFDR: usize = 0x04;
const I2CR: usize = 0x08;
const I2SR: usize = 0x0c;
const I2DR: usize = 0x10;

const I2CR_IEN: u16 = 1 << 7;
const I2CR_MSTA: u16 = 1 << 5;
const I2CR_MTX: u16 = 1 << 4;
const I2CR_TXAK: u16 = 1 << 3;
const I2CR_RSTA: u16 = 1 << 2;
const I2SR_IBB: u16 = 1 << 5;
const I2SR_IAL: u16 = 1 << 4;
const I2SR_IIF: u16 = 1 << 1;
const I2SR_RXAK: u16 = 1 << 0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    BusBusy,
    Timeout,
    Nak,
    ArbitrationLost,
}

pub type Result<T> = core::result::Result<T, Error>;

/// IFDR code 0x31 = divide by 192: 125 kHz from the 24 MHz perclk, used only when nothing set a rate.
const IFDR_FALLBACK: u16 = 0x31;

impl I2c {
    pub const fn i2c1() -> Self {
        I2c(Mmio::at(0x021a_0000))
    }

    pub fn init(&self) {
        if self.0.read16(IFDR) & 0x3f == 0 {
            self.0.write16(IFDR, IFDR_FALLBACK);
        }
        self.0.write16(I2CR, 0);
        self.0.write16(I2SR, 0);
        self.0.write16(I2CR, I2CR_IEN);
        timer::delay_us(50);
    }

    fn wait_iif(&self) -> Result<()> {
        if !timer::poll_until(10, || self.0.read16(I2SR) & I2SR_IIF != 0) {
            return Err(Error::Timeout);
        }
        let sr = self.0.read16(I2SR);
        self.0.write16(I2SR, 0);
        if sr & I2SR_IAL != 0 {
            return Err(Error::ArbitrationLost);
        }
        Ok(())
    }

    fn tx(&self, b: u8) -> Result<()> {
        self.0.write16(I2DR, b as u16);
        self.wait_iif()?;
        if self.0.read16(I2SR) & I2SR_RXAK != 0 {
            return Err(Error::Nak);
        }
        Ok(())
    }

    fn start(&self) -> Result<()> {
        if !timer::poll_until(10, || self.0.read16(I2SR) & I2SR_IBB == 0) {
            return Err(Error::BusBusy);
        }
        self.0.write16(I2CR, I2CR_IEN | I2CR_MSTA | I2CR_MTX);
        if !timer::poll_until(10, || self.0.read16(I2SR) & I2SR_IBB != 0) {
            return Err(Error::Timeout);
        }
        Ok(())
    }

    fn stop(&self) {
        self.0.write16(I2CR, I2CR_IEN);
        let _ = timer::poll_until(10, || self.0.read16(I2SR) & I2SR_IBB == 0);
    }

    fn guarded<T>(&self, f: impl FnOnce() -> Result<T>) -> Result<T> {
        let r = f();
        self.stop();
        if r.is_err() {
            // Recover a wedged controller for the next transfer.
            self.0.write16(I2CR, 0);
            timer::delay_us(50);
            self.0.write16(I2CR, I2CR_IEN);
        }
        r
    }

    pub fn write_reg(&self, addr: u8, reg: u8, val: u8) -> Result<()> {
        self.guarded(|| {
            self.start()?;
            self.tx(addr << 1)?;
            self.tx(reg)?;
            self.tx(val)
        })
    }

    pub fn read_reg(&self, addr: u8, reg: u8) -> Result<u8> {
        self.guarded(|| {
            self.start()?;
            self.tx(addr << 1)?;
            self.tx(reg)?;
            self.0.write16(I2CR, I2CR_IEN | I2CR_MSTA | I2CR_MTX | I2CR_RSTA);
            self.tx((addr << 1) | 1)?;
            // One byte: receive with NAK, then STOP before reading the data register.
            self.0.write16(I2CR, I2CR_IEN | I2CR_MSTA | I2CR_TXAK);
            let _ = self.0.read16(I2DR); // dummy read starts the transfer
            self.wait_iif()?;
            self.0.write16(I2CR, I2CR_IEN | I2CR_TXAK);
            let _ = timer::poll_until(10, || self.0.read16(I2SR) & I2SR_IBB == 0);
            Ok(self.0.read16(I2DR) as u8)
        })
    }

    pub fn update_reg(&self, addr: u8, reg: u8, clear: u8, set: u8) -> Result<u8> {
        let v = self.read_reg(addr, reg)?;
        let n = (v & !clear) | set;
        if n != v {
            self.write_reg(addr, reg, n)?;
        }
        Ok(n)
    }
}
