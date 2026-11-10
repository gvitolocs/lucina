//! [`Smbus`] on a `lucina-geni` I2C master: a word read is the register byte and a repeated start, then two bytes; a
//! word write is the register byte and two bytes, little endian.

use lucina_geni::{Clock, Error, I2c, Msg, Regs};

use crate::Smbus;

pub struct GeniBus<R, C> {
    pub regs: R,
    pub clock: C,
    pub i2c: I2c,
}

impl<R: Regs, C: Clock> crate::pdic::RegRead for GeniBus<R, C> {
    type Error = Error;

    fn read_reg(&mut self, addr: u8, reg: &[u8], buf: &mut [u8]) -> Result<(), Error> {
        self.i2c.transfer(
            &mut self.regs,
            &mut self.clock,
            addr,
            &mut [Msg::Write(reg), Msg::Read(buf)],
        )
    }
}

impl<R: Regs, C: Clock> crate::pdic::RegWrite for GeniBus<R, C> {
    fn write_reg(&mut self, addr: u8, reg: &[u8], data: &[u8]) -> Result<(), Error> {
        let mut buf = [0u8; 8];
        let len = reg.len() + data.len();
        if len > buf.len() {
            return Err(Error::Stuck);
        }
        buf[..reg.len()].copy_from_slice(reg);
        buf[reg.len()..len].copy_from_slice(data);
        self.i2c.transfer(&mut self.regs, &mut self.clock, addr, &mut [Msg::Write(&buf[..len])])
    }
}

impl<R: Regs, C: Clock> Smbus for GeniBus<R, C> {
    type Error = Error;

    fn read_word(&mut self, addr: u16, reg: u8) -> Result<u16, Error> {
        let mut word = [0u8; 2];
        self.i2c.transfer(
            &mut self.regs,
            &mut self.clock,
            addr as u8,
            &mut [Msg::Write(&[reg]), Msg::Read(&mut word)],
        )?;
        Ok(u16::from_le_bytes(word))
    }

    fn write_word(&mut self, addr: u16, reg: u8, value: u16) -> Result<(), Error> {
        let [lo, hi] = value.to_le_bytes();
        self.i2c.transfer(
            &mut self.regs,
            &mut self.clock,
            addr as u8,
            &mut [Msg::Write(&[reg, lo, hi])],
        )
    }

    /// An SMBus block read: the gauge's first byte is the length. The transfer is always the whole buffer: a smart
    /// battery pads a short block.
    fn read_block(&mut self, addr: u16, reg: u8, buf: &mut [u8]) -> Option<Result<(), Error>> {
        Some(self.i2c.transfer(
            &mut self.regs,
            &mut self.clock,
            addr as u8,
            &mut [Msg::Write(&[reg]), Msg::Read(buf)],
        ))
    }

    /// A NACK or a lost arbitration leaves the bus usable, and fails fast. A timeout has already burnt its second.
    fn may_retry(error: &Error) -> bool {
        matches!(error, Error::Nack { .. } | Error::ArbitrationLost)
    }
}
