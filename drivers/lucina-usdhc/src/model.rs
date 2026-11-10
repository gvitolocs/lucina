//! A uSDHC with an eMMC behind it, for the tests: the rules of the facts in the driver's sources, as far as they matter here.

extern crate std;

use std::{cell::RefCell, rc::Rc, vec, vec::Vec};

use crate::{
    card::{EXT_CSD_SIZE, RCA, ext_csd},
    host::{Clock, DmaArea, Regs},
    regs::*,
};

pub const MEM_SIZE: usize = 4 * 1024 * 1024;
pub const DMA_AREA: u32 = 0x10_0000;
pub const SECTORS: u32 = 7_634_944;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Idle,
    Ready,
    Ident,
    Stby,
    Tran,
    /// Sending data until CMD12.
    Data,
}

pub struct Model {
    pub now: u64,
    pub regs: [u32; 64],
    /// What the card's card clock would be, from the last write.
    pub mem: Vec<u8>,
    pub ext: [u8; EXT_CSD_SIZE],
    pub state: State,
    pub busy_until: u64,
    pub commands: Vec<(u8, u32)>,
    /// `(start, bytes)` of each transfer.
    pub transfers: Vec<(u32, usize)>,
    pub violations: Vec<&'static str>,
    pub ocr_busy_polls: u32,
    pub card_width: u32,
    pub hs_timing: u8,
    pub deny_8bit: bool,
    pub deny_4bit: bool,
    /// The next this many reads fail with a data CRC error.
    pub data_errors: u32,
    /// Commands that get no answer.
    pub silent: Option<u8>,
    pub full_resets: u32,
    pub line_resets: u32,
    pub never_ready: bool,
    /// CMD0 leaves the bus as it was.
    pub cmd0_keeps_bus: bool,
    /// What was written: `(sector, its bytes)`, in order.
    pub written: Vec<(u32, Vec<u8>)>,
    /// The next this many writes fail with a data CRC error.
    pub write_errors: u32,
    /// CMD13s that say the card is still programming after a write.
    pub programming_polls: u32,
}

/// What a sector holds: its number, in every word.
pub fn pattern(lba: u32, buf: &mut [u8]) {
    for chunk in buf.chunks_mut(4) {
        chunk.copy_from_slice(&lba.to_le_bytes()[..chunk.len()]);
    }
}

impl Model {
    pub fn new() -> Self {
        let mut ext = [0u8; EXT_CSD_SIZE];
        ext[ext_csd::REV] = 6;
        ext[ext_csd::STRUCTURE] = 2;
        ext[ext_csd::CARD_TYPE] = 0x7;
        ext[ext_csd::SEC_COUNT..ext_csd::SEC_COUNT + 4].copy_from_slice(&SECTORS.to_le_bytes());
        ext[ext_csd::BOOT_SIZE_MULT] = 16;
        ext[ext_csd::RPMB_SIZE_MULT] = 4;
        let mut regs = [0u32; 64];
        regs[SYS_CTRL / 4] = 0x0080_800f;
        regs[PROT_CTRL / 4] = 0x0880_0020;
        regs[MIX_CTRL / 4] = 0x8000_0000;
        Self {
            now: 0,
            regs,
            mem: vec![0; MEM_SIZE],
            ext,
            state: State::Idle,
            busy_until: 0,
            commands: Vec::new(),
            transfers: Vec::new(),
            violations: Vec::new(),
            ocr_busy_polls: 2,
            card_width: 0,
            hs_timing: 0,
            deny_8bit: false,
            deny_4bit: false,
            data_errors: 0,
            silent: None,
            full_resets: 0,
            line_resets: 0,
            never_ready: false,
            cmd0_keeps_bus: false,
            written: Vec::new(),
            write_errors: 0,
            programming_polls: 0,
        }
    }

    fn reg(&self, offset: usize) -> u32 {
        self.regs[offset / 4]
    }

    fn set(&mut self, offset: usize, value: u32) {
        self.regs[offset / 4] = value;
    }

    fn raise(&mut self, bits: u32) {
        self.regs[INT_STATUS / 4] |= bits;
    }

    pub fn host_width(&self) -> u32 {
        match self.reg(PROT_CTRL) & PROT_DTW_MASK {
            PROT_DTW_8 => 2,
            PROT_DTW_4 => 1,
            _ => 0,
        }
    }

    /// The bits of a card status, for R1.
    fn status(&self) -> u32 {
        let state = match self.state {
            State::Idle => 0,
            State::Ready => 1,
            State::Ident => 2,
            State::Stby => 3,
            State::Tran => 4,
            State::Data => 5,
        };
        (state << 9) | (1 << 8)
    }

    fn read32(&mut self, offset: usize) -> u32 {
        self.now += 1;
        match offset {
            PRES_STATE => {
                let mut v = self.reg(PRES_STATE) & !PS_DAT0;
                v |= PS_CLOCK_STABLE;
                if self.reg(SYS_CTRL) & SYS_CLOCKS_ON != 7 {
                    v |= PS_CLOCK_OFF;
                }
                if self.now >= self.busy_until {
                    v |= PS_DAT0;
                }
                v
            }
            _ => self.reg(offset),
        }
    }

    fn write32(&mut self, offset: usize, value: u32) {
        self.now += 1;
        match offset {
            INT_STATUS => self.regs[INT_STATUS / 4] &= !value,
            SYS_CTRL => {
                if self.reg(SYS_CTRL) & SYS_IPP_RST_N != 0 && value & SYS_IPP_RST_N == 0 {
                    self.violations.push("the eMMC's reset line was pulled low");
                }
                let mut value = value;
                if value & SYS_RSTA != 0 {
                    if self.state == State::Data || self.reg(PRES_STATE) & PS_DATA_INHIBIT != 0 {
                        self.violations.push("a full reset during a transfer");
                    }
                    self.full_resets += 1;
                    // Clears the clock enables, leaves MIX_CTRL.
                    value = 0x0080_8000;
                    self.set(INT_STATUS_EN, 0);
                    self.set(PROT_CTRL, 0x0880_0020);
                }
                if value & (SYS_RSTC | SYS_RSTD) != 0 {
                    self.line_resets += 1;
                    value &= !(SYS_RSTC | SYS_RSTD);
                    // A data reset loses the bus width.
                    let prot = self.reg(PROT_CTRL) & !PROT_DTW_MASK;
                    self.set(PROT_CTRL, prot);
                }
                self.set(SYS_CTRL, value);
            }
            CMD_XFR_TYP => self.command(value),
            _ => self.set(offset, value),
        }
    }

    fn command(&mut self, xfer: u32) {
        let cmd = (xfer >> CMD_INDEX_SHIFT) as u8 & 0x3f;
        let arg = self.reg(CMD_ARG);
        self.commands.push((cmd, arg));

        if self.reg(PRES_STATE) & PS_CMD_INHIBIT != 0 {
            self.violations.push("a command while the command line is taken");
        }
        if self.silent == Some(cmd) {
            self.raise(1 << 16);
            return;
        }
        let rsp = xfer & (3 << 16);
        let mut response = [0u32; 4];
        let mut ok = true;
        match (cmd, self.state) {
            (0, _) => {
                self.state = State::Idle;
                if !self.cmd0_keeps_bus {
                    self.card_width = 0;
                    self.hs_timing = 0;
                }
            }
            (1, State::Idle | State::Ready) => {
                // A card answers its ranges, and is ready only after polls.
                let ready = self.ocr_busy_polls == 0 && !self.never_ready && arg != 0;
                if arg != 0 && self.ocr_busy_polls > 0 {
                    self.ocr_busy_polls -= 1;
                }
                response[0] = 0x40ff_8080 | if ready { 1 << 31 } else { 0 };
                self.state = if ready { State::Ready } else { State::Idle };
            }
            (2, State::Ready) => {
                // A CID: manufacturer 0x11, name "016G92", serial 0x12345678.
                response = [0x5678_0000 | 0x00ab, 0x3932_1234, 0x3136_4700, 0x0011_0030];
                self.state = State::Ident;
            }
            (3, State::Ident) => {
                if arg != (RCA as u32) << 16 {
                    ok = false;
                }
                response[0] = self.status();
                self.state = State::Stby;
            }
            (7, State::Stby) => {
                response[0] = self.status();
                self.state = State::Tran;
                self.busy_until = self.now + 40;
            }
            (13, _) => {
                response[0] = self.status();
                if self.programming_polls > 0 {
                    self.programming_polls -= 1;
                    response[0] &= !(1 << 8);
                }
            }
            (6, State::Tran) => {
                response[0] = self.status();
                let (index, value) = ((arg >> 16) as u8 as usize, (arg >> 8) as u8);
                match index {
                    ext_csd::BUS_WIDTH => self.card_width = value as u32,
                    ext_csd::HS_TIMING => self.hs_timing = value,
                    ext_csd::CACHE_CTRL => self.ext[ext_csd::CACHE_CTRL] = value,
                    _ => self.violations.push("a CMD6 to another byte"),
                }
                self.busy_until = self.now + 40;
            }
            (8, State::Tran) => {
                response[0] = self.status();
                if self.bus_bad() {
                    self.raise(INT_CMD_DONE | (1 << 21));
                    return;
                }
                let at = self.reg(DS_ADDR) as usize;
                let ext = self.ext;
                self.mem[at..at + EXT_CSD_SIZE].copy_from_slice(&ext);
                self.check_data(EXT_CSD_SIZE as u32, 1, true);
                self.transfers.push((at as u32, EXT_CSD_SIZE));
                self.raise(INT_DATA_DONE);
            }
            (17 | 18, State::Tran) => {
                response[0] = self.status();
                let blocks = self.reg(BLK_ATT) >> 16;
                if (blocks > 1) != (cmd == 18) {
                    self.violations.push("a block count that does not match the command");
                }
                if cmd == 18 {
                    self.state = State::Data;
                }
                if self.bus_bad() || self.data_errors > 0 {
                    self.data_errors = self.data_errors.saturating_sub(1);
                    self.raise(INT_CMD_DONE | (1 << 21));
                    return;
                }
                let at = self.reg(DS_ADDR) as usize;
                let bytes = blocks as usize * 512;
                self.check_data(512, blocks, true);
                if at / crate::host::BOUNDARY != (at + bytes - 1) / crate::host::BOUNDARY {
                    self.violations.push("a transfer over a DMA boundary");
                }
                for i in 0..blocks as usize {
                    let mut sector = [0u8; 512];
                    pattern(arg + i as u32, &mut sector);
                    self.mem[at + i * 512..at + (i + 1) * 512].copy_from_slice(&sector);
                }
                self.transfers.push((at as u32, bytes));
                self.raise(INT_DATA_DONE);
            }
            (24 | 25, State::Tran) => {
                response[0] = self.status();
                let blocks = self.reg(BLK_ATT) >> 16;
                if (blocks > 1) != (cmd == 25) {
                    self.violations.push("a block count that does not match the command");
                }
                if cmd == 25 {
                    self.state = State::Data;
                }
                if self.bus_bad() || self.write_errors > 0 {
                    self.write_errors = self.write_errors.saturating_sub(1);
                    self.raise(INT_CMD_DONE | (1 << 21));
                    return;
                }
                let at = self.reg(DS_ADDR) as usize;
                let bytes = blocks as usize * 512;
                self.check_data(512, blocks, false);
                if at / crate::host::BOUNDARY != (at + bytes - 1) / crate::host::BOUNDARY {
                    self.violations.push("a transfer over a DMA boundary");
                }
                for i in 0..blocks as usize {
                    self.written
                        .push((arg + i as u32, self.mem[at + i * 512..at + (i + 1) * 512].to_vec()));
                }
                self.transfers.push((at as u32, bytes));
                self.busy_until = self.now + 40;
                self.raise(INT_DATA_DONE);
            }
            (12, State::Tran | State::Data) => {
                if xfer & CMD_TYPE_ABORT != CMD_TYPE_ABORT {
                    self.violations.push("a CMD12 that is not an abort");
                }
                response[0] = self.status();
                self.state = State::Tran;
                self.busy_until = self.now + 40;
            }
            _ => ok = false,
        }
        if !ok {
            // The card does not answer in this state.
            self.raise(1 << 16);
            return;
        }
        if rsp != 0 {
            for (i, word) in response.iter().enumerate() {
                self.set(CMD_RSP0 + i * 4, *word);
            }
        }
        self.raise(INT_CMD_DONE);
    }

    /// Whether the host's bus and the card's differ, or the card has no such bus on this board.
    fn bus_bad(&self) -> bool {
        self.host_width() != self.card_width
            || (self.card_width == 2 && self.deny_8bit)
            || (self.card_width == 1 && self.deny_4bit)
    }

    fn check_data(&mut self, block_size: u32, blocks: u32, read: bool) {
        if self.reg(BLK_ATT) != (blocks << 16) | block_size {
            self.violations.push("BLK_ATT is not what the command needs");
        }
        let mix = self.reg(MIX_CTRL);
        if mix & MIX_DMA == 0 || (mix & MIX_READ != 0) != read || mix & MIX_BLOCK_COUNT == 0 {
            self.violations.push("MIX_CTRL does not say this DMA");
        }
        if (mix & MIX_MULTI != 0) != (blocks > 1) {
            self.violations.push("MIX_CTRL's multi-block bit does not match");
        }
        let prot = self.reg(PROT_CTRL);
        if prot & PROT_DMASEL_MASK != 0 {
            self.violations.push("not SDMA");
        }
        if prot & PROT_EMODE_MASK != PROT_EMODE_LE {
            self.violations.push("not little endian");
        }
        if self.reg(DS_ADDR) % 4 != 0 {
            self.violations.push("an address that is not at 4 bytes");
        }
    }
}

#[derive(Clone)]
pub struct Shared(pub Rc<RefCell<Model>>);

impl Regs for Shared {
    fn read32(&self, offset: usize) -> u32 {
        self.0.borrow_mut().read32(offset)
    }
    fn write32(&self, offset: usize, value: u32) {
        self.0.borrow_mut().write32(offset, value);
    }
}

impl DmaArea for Shared {
    fn daddr(&self) -> u32 {
        DMA_AREA
    }
    fn read(&self, offset: usize, buf: &mut [u8]) {
        let at = DMA_AREA as usize + offset;
        buf.copy_from_slice(&self.0.borrow().mem[at..at + buf.len()]);
    }
}

impl Clock for Shared {
    fn now_us(&self) -> u64 {
        let mut model = self.0.borrow_mut();
        model.now += 1;
        model.now
    }
    fn delay_us(&self, us: u64) {
        self.0.borrow_mut().now += us;
    }
}
