//! The master side of an i.MX I2C controller, polled: for the PMIC here, and for the touch panel on the other bus.
//!
//! It is the procedure `lucina-ci-gadget` uses on I2C1 for the USB port's supply. The kernel must not run the two at once
//! on one bus.

use alloc::vec::Vec;

use crate::Error;

/// The controllers' base addresses.
pub const I2C1: usize = 0x021a_0000;
pub const I2C2: usize = 0x021a_4000;

/// Access to one controller's registers, which are 16 bits wide, and time.
pub trait I2cHw {
    fn read16(&mut self, addr: usize) -> u16;
    fn write16(&mut self, addr: usize, val: u16);
    fn delay_us(&mut self, us: u32);
}

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
/// Dividing by 192: under 400 kHz from any clock this block has, for a kernel that set no rate.
const IFDR_FALLBACK: u16 = 0x31;

const POLL_LIMIT: u32 = 1000;
const POLL_STEP_US: u32 = 10;

/// One controller.
pub struct I2c<H> {
    hw: H,
    base: usize,
}

impl<H: I2cHw> I2c<H> {
    pub fn new(hw: H, base: usize) -> Self {
        Self { hw, base }
    }

    pub fn hw(&mut self) -> &mut H {
        &mut self.hw
    }

    fn read(&mut self, reg: usize) -> u16 {
        self.hw.read16(self.base + reg)
    }

    fn write(&mut self, reg: usize, val: u16) {
        self.hw.write16(self.base + reg, val);
    }

    fn poll(&mut self, mut done: impl FnMut(&mut Self) -> bool) -> bool {
        for _ in 0..POLL_LIMIT {
            if done(self) {
                return true;
            }
            self.hw.delay_us(POLL_STEP_US);
        }
        done(self)
    }

    /// Turns the controller on, keeping a rate the previous kernel set.
    pub fn init(&mut self) {
        if self.read(IFDR) & 0x3f == 0 {
            self.write(IFDR, IFDR_FALLBACK);
        }
        self.write(I2CR, 0);
        self.write(I2SR, 0);
        self.write(I2CR, I2CR_IEN);
        self.hw.delay_us(50);
    }

    fn wait_transfer(&mut self) -> Result<(), Error> {
        if !self.poll(|bus| bus.read(I2SR) & I2SR_IIF != 0) {
            return Err(Error::I2cTimeout);
        }
        let status = self.read(I2SR);
        self.write(I2SR, 0);
        if status & I2SR_IAL != 0 {
            return Err(Error::I2cTimeout);
        }
        Ok(())
    }

    fn send(&mut self, byte: u8) -> Result<(), Error> {
        self.write(I2DR, u16::from(byte));
        self.wait_transfer()?;
        if self.read(I2SR) & I2SR_RXAK != 0 {
            return Err(Error::I2cNak);
        }
        Ok(())
    }

    fn start(&mut self) -> Result<(), Error> {
        if !self.poll(|bus| bus.read(I2SR) & I2SR_IBB == 0) {
            return Err(Error::I2cTimeout);
        }
        self.write(I2CR, I2CR_IEN | I2CR_MSTA | I2CR_MTX);
        if self.poll(|bus| bus.read(I2SR) & I2SR_IBB != 0) {
            Ok(())
        } else {
            Err(Error::I2cTimeout)
        }
    }

    fn stop(&mut self) {
        self.write(I2CR, I2CR_IEN);
        let _ = self.poll(|bus| bus.read(I2SR) & I2SR_IBB == 0);
    }

    /// Ends a transaction, resetting the controller after a failure for the next one.
    fn finish<T>(&mut self, result: Result<T, Error>) -> Result<T, Error> {
        self.stop();
        if result.is_err() {
            self.write(I2CR, 0);
            self.hw.delay_us(50);
            self.write(I2CR, I2CR_IEN);
        }
        result
    }

    /// Writes bytes to a device, the first ones usually naming its register.
    pub fn write_bytes(&mut self, addr: u8, bytes: &[u8]) -> Result<(), Error> {
        self.start()?;
        let result = (|| {
            self.send(addr << 1)?;
            for &byte in bytes {
                self.send(byte)?;
            }
            Ok(())
        })();
        self.finish(result)
    }

    /// Writes `prefix`, then reads `len` bytes after a repeated start.
    pub fn read_bytes(&mut self, addr: u8, prefix: &[u8], len: usize) -> Result<Vec<u8>, Error> {
        let mut out = Vec::with_capacity(len);
        self.start()?;
        let result = (|| {
            self.send(addr << 1)?;
            for &byte in prefix {
                self.send(byte)?;
            }
            self.write(I2CR, I2CR_IEN | I2CR_MSTA | I2CR_MTX | I2CR_RSTA);
            self.send((addr << 1) | 1)?;
            // Into receive: the first read only starts the bus, and the last byte is not acknowledged.
            let nak_first = if len == 1 { I2CR_TXAK } else { 0 };
            self.write(I2CR, I2CR_IEN | I2CR_MSTA | nak_first);
            let _ = self.read(I2DR);
            for index in 0..len {
                self.wait_transfer()?;
                if index + 2 == len {
                    self.write(I2CR, I2CR_IEN | I2CR_MSTA | I2CR_TXAK);
                }
                if index + 1 == len {
                    // A stop before the read, or the controller clocks another byte in.
                    self.write(I2CR, I2CR_IEN | I2CR_TXAK);
                    let _ = self.poll(|bus| bus.read(I2SR) & I2SR_IBB == 0);
                }
                out.push(self.read(I2DR) as u8);
            }
            Ok(())
        })();
        self.finish(result).map(|()| out)
    }

    /// One register of a device with 8-bit register numbers.
    pub fn read_reg(&mut self, addr: u8, reg: u8) -> Result<u8, Error> {
        Ok(self.read_bytes(addr, &[reg], 1)?[0])
    }

    /// Writes one register of a device with 8-bit register numbers.
    pub fn write_reg(&mut self, addr: u8, reg: u8, val: u8) -> Result<(), Error> {
        self.write_bytes(addr, &[reg, val])
    }
}
