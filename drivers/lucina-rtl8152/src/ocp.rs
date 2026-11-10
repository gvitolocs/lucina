//! Register access (U-Boot `generic_ocp_read`, `generic_ocp_write`, `ocp_read_*`, `ocp_write_*`, `ocp_reg_*`).
//!
//! Everything is a 4-byte-aligned vendor control transfer: the address in `wValue`, the register space and the byte
//! enables in `wIndex`. A narrower write sends the whole dword with the value shifted to its place and only that
//! place's bytes enabled. The PHY has no bus of its own: its registers appear in a window at 0xb000 of the PLA space,
//! with the upper address bits in PLA_OCP_GPHY_BASE.

use crate::driver::Error;
use crate::regs::*;
use crate::transport::{UsbTransport, VendorRequest};

/// U-Boot's burst sizes. A control read of more than 64 bytes is split, a write of more than 512.
const READ_BURST: usize = 64;
const WRITE_BURST: usize = 512;

pub(crate) struct Ocp<T> {
    pub(crate) usb: T,
    /// What PLA_OCP_GPHY_BASE holds, once the driver has written it.
    base: Option<u16>,
}

impl<T: UsbTransport> Ocp<T> {
    pub(crate) fn new(usb: T) -> Self {
        Self { usb, base: None }
    }

    fn set_registers(&mut self, value: u16, index: u16, data: &[u8]) -> Result<(), Error> {
        let req = VendorRequest {
            request: REQ_REGS,
            value,
            index,
        };
        let actual = self.usb.control_out(req, data)?;
        if actual != data.len() {
            return Err(Error::ShortTransfer {
                expected: data.len(),
                actual,
            });
        }
        Ok(())
    }

    /// `pla_ocp_read` / `usb_ocp_read`. `addr` and the length must be multiples of 4.
    pub(crate) fn read(&mut self, mcu: Mcu, addr: u16, data: &mut [u8]) -> Result<(), Error> {
        if data.is_empty()
            || !data.len().is_multiple_of(4)
            || !addr.is_multiple_of(4)
            || addr as usize + data.len() > 0xffff
        {
            return Err(Error::BadAccess { addr });
        }
        let mut value = addr;
        for chunk in data.chunks_mut(READ_BURST) {
            let req = VendorRequest {
                request: REQ_REGS,
                value,
                index: mcu.index(),
            };
            let actual = self.usb.control_in(req, chunk)?;
            if actual != chunk.len() {
                return Err(Error::ShortTransfer {
                    expected: chunk.len(),
                    actual,
                });
            }
            value += chunk.len() as u16;
        }
        Ok(())
    }

    /// `generic_ocp_write`: the first dword with the low nibble of `byte_en` (repeated in both nibbles, as the chip
    /// wants it), the dwords in between with all bytes on, the last dword with the high nibble.
    pub(crate) fn write(
        &mut self,
        mcu: Mcu,
        addr: u16,
        byte_en: u16,
        data: &[u8],
    ) -> Result<(), Error> {
        if data.is_empty()
            || !data.len().is_multiple_of(4)
            || !addr.is_multiple_of(4)
            || addr as usize + data.len() > 0xffff
        {
            return Err(Error::BadAccess { addr });
        }
        let start = byte_en & BYTE_EN_START_MASK;
        let end = byte_en & BYTE_EN_END_MASK;
        let (first, rest) = data.split_at(4);
        self.set_registers(addr, mcu.index() | start | (start << 4), first)?;
        let mut value = addr + 4;
        if !rest.is_empty() {
            let (middle, last) = rest.split_at(rest.len() - 4);
            for chunk in middle.chunks(WRITE_BURST) {
                self.set_registers(value, mcu.index() | BYTE_EN_DWORD, chunk)?;
                value += chunk.len() as u16;
            }
            self.set_registers(value, mcu.index() | end | (end >> 4), last)?;
        }
        Ok(())
    }

    pub(crate) fn read_dword(&mut self, mcu: Mcu, addr: u16) -> Result<u32, Error> {
        let mut raw = [0u8; 4];
        self.read(mcu, addr & !3, &mut raw)?;
        Ok(u32::from_le_bytes(raw))
    }

    pub(crate) fn write_dword(&mut self, mcu: Mcu, addr: u16, value: u32) -> Result<(), Error> {
        self.write(mcu, addr, BYTE_EN_DWORD, &value.to_le_bytes())
    }

    pub(crate) fn read_word(&mut self, mcu: Mcu, addr: u16) -> Result<u16, Error> {
        let shift = (addr & 2) as u32 * 8;
        Ok((self.read_dword(mcu, addr)? >> shift) as u16)
    }

    pub(crate) fn write_word(&mut self, mcu: Mcu, addr: u16, value: u16) -> Result<(), Error> {
        let shift = addr & 2;
        let data = (value as u32) << (shift * 8);
        self.write(mcu, addr & !3, BYTE_EN_WORD << shift, &data.to_le_bytes())
    }

    pub(crate) fn read_byte(&mut self, mcu: Mcu, addr: u16) -> Result<u8, Error> {
        let shift = (addr & 3) as u32 * 8;
        Ok((self.read_dword(mcu, addr)? >> shift) as u8)
    }

    pub(crate) fn write_byte(&mut self, mcu: Mcu, addr: u16, value: u8) -> Result<(), Error> {
        let shift = addr & 3;
        let data = (value as u32) << (shift * 8);
        self.write(mcu, addr & !3, BYTE_EN_BYTE << shift, &data.to_le_bytes())
    }

    fn select_phy_page(&mut self, addr: u16) -> Result<u16, Error> {
        let base = addr & 0xf000;
        if self.base != Some(base) {
            self.write_word(Mcu::Pla, PLA_OCP_GPHY_BASE, base)?;
            self.base = Some(base);
        }
        Ok((addr & 0x0fff) | OCP_WINDOW)
    }

    /// `ocp_reg_read`
    pub(crate) fn reg_read(&mut self, addr: u16) -> Result<u16, Error> {
        let index = self.select_phy_page(addr)?;
        self.read_word(Mcu::Pla, index)
    }

    /// `ocp_reg_write`
    pub(crate) fn reg_write(&mut self, addr: u16, value: u16) -> Result<(), Error> {
        let index = self.select_phy_page(addr)?;
        self.write_word(Mcu::Pla, index, value)
    }

    pub(crate) fn mdio_read(&mut self, reg: u16) -> Result<u16, Error> {
        self.reg_read(OCP_BASE_MII + reg * 2)
    }

    pub(crate) fn mdio_write(&mut self, reg: u16, value: u16) -> Result<(), Error> {
        self.reg_write(OCP_BASE_MII + reg * 2, value)
    }
}
