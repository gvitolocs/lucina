//! A controller and card to test the driver against: what Linux's `sdhci-msm.c` and the SD specifications expect, plus
//! the faults the tests ask for.

use alloc::{collections::BTreeMap, rc::Rc, vec, vec::Vec};
use core::cell::{Cell, RefCell};

use crate::{
    host::{Clock, DmaArea, Regs},
    msm,
    regs::*,
};

/// A shared time base: every look at the clock moves it a little, so that a bug cannot hang the tests. The model decides
/// whether there is an interrupt to sleep on and counts those sleeps.
#[derive(Clone)]
pub struct TestClock(Rc<Cell<u64>>, pub Model);

impl TestClock {
    pub fn new(model: &Model) -> Self {
        TestClock(Rc::new(Cell::new(0)), model.clone())
    }
}

impl Clock for TestClock {
    fn now_us(&self) -> u64 {
        self.0.set(self.0.get() + 1);
        self.0.get()
    }
    fn delay_us(&self, us: u64) {
        self.0.set(self.0.get() + us);
    }
    fn wait_interrupt(&self, timeout_us: u64) -> bool {
        let mut st = self.1.0.borrow_mut();
        if !st.irq {
            return false;
        }
        st.irq_waits += 1;
        assert!(timeout_us >= 1);
        self.0.set(self.0.get() + 1);
        true
    }
}

pub const DMA_BASE: u64 = 0x8000_0000;
pub const SD_SECTOR: usize = 512;

#[derive(Clone)]
pub struct Model(pub Rc<RefCell<State>>);

pub struct State {
    // Controller.
    pub version: u16,
    pub caps: u32,
    pub qcom: bool,
    pub regs: BTreeMap<usize, u8>,
    pub writes: Vec<(usize, u64)>,
    pub int_status: u32,
    /// Raised once the driver has acknowledged the response: the data phase comes after it.
    pub pending_int: u32,
    pub adma_addr: u64,
    pub reset_all_count: u32,
    /// A pending power request holds RESET_ALL (what Scarlet saw with its interrupt masked).
    pub reset_waits_for_pwr: bool,
    pub reset_all_stuck: bool,
    pub reset_in_progress: bool,
    pub pwr_status: u32,
    pub pwr_ctl_writes: Vec<u32>,
    pub power_written: Vec<u8>,
    // Card.
    pub card_inserted: bool,
    pub write_switch_on: bool,
    pub high_capacity: bool,
    pub v2: bool,
    pub store: Vec<u8>,
    pub acmd41_polls: u32,
    pub rca: u16,
    pub selected: bool,
    pub app: bool,
    pub bus_4bit: bool,
    pub high_speed: bool,
    pub csd: [u32; 4],
    pub scr: [u8; 8],
    pub commands: Vec<(u8, u32)>,
    pub fail_data_of: Option<u8>,
    /// The data phase of this command never ends: the response comes, then neither DATA_END nor an error.
    pub hang_data_of: Option<u8>,
    /// A command or data line reset never self-clears.
    pub lines_reset_stuck: bool,
    /// The line reset bits last written.
    pub lines_reset: u8,
    pub fail_scr: bool,
    pub card_busy_polls: u32,
    /// The next command's response is latched this many `INT_STATUS` reads late; until then the register reads it as
    /// not there.
    pub rsp_lag: u32,
    /// A response that is being lagged: its status and how many reads are left.
    pending_rsp: Option<(u32, u32)>,
    /// The kernel has the controller's interrupt: [`Clock::wait_interrupt`] sleeps (here: counts and lets time pass).
    pub irq: bool,
    /// Interrupt waits the host slept.
    pub irq_waits: u32,
    // System memory.
    pub mem: BTreeMap<u64, u8>,
}

impl State {
    fn r8(&self, off: usize) -> u8 {
        *self.regs.get(&off).unwrap_or(&0)
    }
    fn w8(&mut self, off: usize, v: u8) {
        self.regs.insert(off, v);
    }
    pub fn r32(&self, off: usize) -> u32 {
        u32::from_le_bytes([
            self.r8(off),
            self.r8(off + 1),
            self.r8(off + 2),
            self.r8(off + 3),
        ])
    }
    fn w32(&mut self, off: usize, v: u32) {
        for (i, b) in v.to_le_bytes().iter().enumerate() {
            self.w8(off + i, *b);
        }
    }
    fn set_response(&mut self, words: [u32; 4]) {
        for (i, w) in words.iter().enumerate() {
            self.w32(RESPONSE + i * 4, *w);
        }
    }
    fn set_long(&mut self, raw: [u32; 4]) {
        // Bits 127-8 go in the registers, raw[0] holding the top.
        self.set_response([
            (raw[3] >> 8) | (raw[2] << 24),
            (raw[2] >> 8) | (raw[1] << 24),
            (raw[1] >> 8) | (raw[0] << 24),
            raw[0] >> 8,
        ]);
    }
    fn r1(&self) -> u32 {
        let state = if self.selected { 4 } else { 3 };
        (state << 9) | (1 << 8)
    }
    fn table(&self) -> Vec<(u64, usize)> {
        let adma64 = self.r8(HOST_CONTROL) & CTRL_DMA_MASK == CTRL_ADMA64;
        let line = if adma64 { 12 } else { 8 };
        let mut at = self.adma_addr;
        let mut out = Vec::new();
        loop {
            let get = |o: u64| *self.mem.get(&(at + o)).unwrap_or(&0);
            let attr = u16::from_le_bytes([get(0), get(1)]);
            let len = u16::from_le_bytes([get(2), get(3)]) as usize;
            let mut addr = u32::from_le_bytes([get(4), get(5), get(6), get(7)]) as u64;
            if adma64 {
                addr |= (u32::from_le_bytes([get(8), get(9), get(10), get(11)]) as u64) << 32;
            }
            assert!(attr & ADMA_ATTR_VALID != 0, "an invalid ADMA line");
            assert_eq!(attr & (3 << 4), ADMA_ATTR_TRAN, "not a transfer line");
            out.push((addr, len));
            if attr & ADMA_ATTR_END != 0 {
                return out;
            }
            at += line as u64;
        }
    }
    fn dma_in(&mut self, data: &[u8]) {
        let mut done = 0;
        for (addr, len) in self.table() {
            for i in 0..len {
                if done == data.len() {
                    return;
                }
                self.mem.insert(addr + i as u64, data[done]);
                done += 1;
            }
        }
    }
    fn dma_out(&self, n: usize) -> Vec<u8> {
        let mut out = Vec::new();
        for (addr, len) in self.table() {
            for i in 0..len {
                if out.len() == n {
                    return out;
                }
                out.push(*self.mem.get(&(addr + i as u64)).unwrap_or(&0));
            }
        }
        out
    }
    fn run_command(&mut self, value: u16) {
        let cmd = (value >> 8) as u8;
        let arg = self.r32(ARGUMENT);
        self.commands.push((cmd, arg));
        let app = core::mem::take(&mut self.app);
        let mode = u16::from_le_bytes([self.r8(TRANSFER_MODE), self.r8(TRANSFER_MODE + 1)]);
        let block_size = u16::from_le_bytes([self.r8(BLOCK_SIZE), self.r8(BLOCK_SIZE + 1)]) & 0xfff;
        let blocks = u16::from_le_bytes([self.r8(BLOCK_COUNT), self.r8(BLOCK_COUNT + 1)]) as usize;
        let mut status = INT_RESPONSE;
        match (app, cmd) {
            (_, 0) => {
                self.selected = false;
                self.rca = 0;
                self.acmd41_polls = 3;
            }
            (_, 8) => {
                if !self.v2 {
                    self.int_status |= INT_ERROR | (1 << 16);
                    return;
                }
                self.set_response([arg & 0xfff, 0, 0, 0]);
            }
            (_, 55) => {
                self.app = true;
                self.set_response([self.r1(), 0, 0, 0]);
            }
            (true, 41) => {
                assert!(arg & crate::card::OCR_VOLTAGES != 0);
                let done = if self.acmd41_polls == 0 {
                    1 << 31
                } else {
                    self.acmd41_polls -= 1;
                    0
                };
                let ccs = if self.high_capacity && arg & (1 << 30) != 0 {
                    1 << 30
                } else {
                    0
                };
                self.set_response([done | ccs | crate::card::OCR_VOLTAGES, 0, 0, 0]);
            }
            (_, 2) => self.set_long([0x0353_4453, 0x4c55_4349_u32, 0x4e41_1234, 0x5678_0000]),
            (_, 3) => {
                self.rca = 0xb368;
                self.set_response([(self.rca as u32) << 16, 0, 0, 0]);
            }
            (_, 9) => {
                assert_eq!(arg >> 16, self.rca as u32);
                let csd = self.csd;
                self.set_long(csd);
            }
            (_, 7) => {
                assert_eq!(arg >> 16, self.rca as u32);
                self.selected = true;
                self.set_response([self.r1(), 0, 0, 0]);
                status |= INT_DATA_END;
            }
            (_, 16) => self.set_response([self.r1(), 0, 0, 0]),
            (_, 13) => {
                let mut r = self.r1();
                if self.card_busy_polls > 0 {
                    self.card_busy_polls -= 1;
                    r &= !(1 << 8);
                }
                self.set_response([r, 0, 0, 0]);
            }
            (true, 6) => {
                assert_eq!(arg, 2);
                self.bus_4bit = true;
                self.set_response([self.r1(), 0, 0, 0]);
            }
            (true, 51) => {
                if self.fail_scr {
                    self.int_status |= INT_ERROR | (1 << 20);
                    return;
                }
                assert_eq!(block_size, 8);
                self.set_response([self.r1(), 0, 0, 0]);
                let scr = self.scr;
                self.dma_in(&scr);
                status |= INT_DATA_END;
            }
            (false, 6) => {
                assert_eq!(block_size, 64);
                let mut s = [0u8; 64];
                s[13] = 0x03;
                if arg & (1 << 31) != 0 {
                    s[16] = 1;
                    self.high_speed = true;
                }
                self.set_response([self.r1(), 0, 0, 0]);
                self.dma_in(&s);
                status |= INT_DATA_END;
            }
            (_, 17 | 18) | (_, 24 | 25) => {
                assert_eq!(block_size as usize, SD_SECTOR);
                let n = if cmd == 17 || cmd == 24 { 1 } else { blocks };
                assert_eq!(cmd == 18 || cmd == 25, mode & TRNS_MULTI != 0);
                if n > 1 {
                    assert!(mode & TRNS_AUTO_CMD12 != 0);
                }
                assert_eq!(mode & TRNS_READ != 0, cmd < 20);
                assert!(mode & TRNS_DMA != 0);
                let start = if self.high_capacity {
                    arg as usize
                } else {
                    arg as usize / SD_SECTOR
                };
                self.set_response([self.r1(), 0, 0, 0]);
                status |= INT_DATA_END;
                if self.fail_data_of == Some(cmd) {
                    self.fail_data_of = None;
                    status = INT_RESPONSE;
                    self.pending_int = INT_ERROR | (1 << 21);
                } else if self.hang_data_of == Some(cmd) {
                    self.hang_data_of = None;
                    status = INT_RESPONSE;
                } else if cmd < 20 {
                    let at = start * SD_SECTOR;
                    let data = self.store[at..at + n * SD_SECTOR].to_vec();
                    self.dma_in(&data);
                } else {
                    let at = start * SD_SECTOR;
                    let data = self.dma_out(n * SD_SECTOR);
                    self.store[at..at + n * SD_SECTOR].copy_from_slice(&data);
                }
            }
            other => panic!("unexpected command {other:?}"),
        }
        if self.rsp_lag > 0 {
            self.pending_rsp = Some((status, self.rsp_lag));
            self.rsp_lag = 0;
        } else {
            self.int_status |= status;
        }
    }
}

impl Model {
    pub fn new(qcom: bool, sectors: usize, high_capacity: bool) -> Self {
        let csd = if high_capacity {
            // Version 1: C_SIZE at bits 69-48, command class 10 set.
            let c_size = (sectors / 1024 - 1) as u32;
            [
                1 << 30,
                (0x5b5u32 << 20) | (9 << 16) | (c_size >> 16),
                c_size << 16,
                0,
            ]
        } else {
            // Version 0: READ_BL_LEN 9, capacity (C_SIZE + 1) << (MULT + 2).
            let mult = 7u32;
            let c_size = (sectors >> (mult + 2)) as u32 - 1;
            [
                0,
                (0x5b5u32 << 20) | (9 << 16) | (c_size >> 2),
                (c_size << 30) | (mult << 15),
                0,
            ]
        };
        let mut state = State {
            version: 0x0002,
            caps: CAN_ADMA2 | CAN_VDD_330 | CAN_64BIT | (100 << 8),
            qcom,
            regs: BTreeMap::new(),
            writes: Vec::new(),
            int_status: 0,
            pending_int: 0,
            adma_addr: 0,
            reset_all_count: 0,
            reset_waits_for_pwr: false,
            reset_all_stuck: false,
            reset_in_progress: false,
            pwr_status: 0,
            pwr_ctl_writes: Vec::new(),
            power_written: Vec::new(),
            card_inserted: true,
            write_switch_on: true,
            high_capacity,
            v2: true,
            store: vec![0; sectors.min(0x4000) * SD_SECTOR],
            acmd41_polls: 3,
            rca: 0,
            selected: false,
            app: false,
            bus_4bit: false,
            high_speed: false,
            csd,
            scr: [0x02, 0x05, 0, 0, 0, 0, 0, 0],
            commands: Vec::new(),
            fail_data_of: None,
            hang_data_of: None,
            lines_reset_stuck: false,
            lines_reset: 0,
            fail_scr: false,
            card_busy_polls: 0,
            rsp_lag: 0,
            pending_rsp: None,
            irq: false,
            irq_waits: 0,
            mem: BTreeMap::new(),
        };
        state.w32(msm::CORE_MCI_VERSION, 0x1000_0071);
        Self(Rc::new(RefCell::new(state)))
    }

    /// The commands the card saw, by index.
    pub fn commands(&self) -> Vec<u8> {
        self.0.borrow().commands.iter().map(|c| c.0).collect()
    }
}

impl Regs for Model {
    fn read32(&self, offset: usize) -> u32 {
        if offset == INT_STATUS {
            let mut s = self.0.borrow_mut();
            if let Some((status, reads)) = s.pending_rsp {
                if reads == 1 {
                    s.int_status |= status;
                    s.pending_rsp = None;
                } else {
                    s.pending_rsp = Some((status, reads - 1));
                }
            }
            return s.int_status;
        }
        let s = self.0.borrow();
        match offset {
            PRESENT_STATE => {
                let mut v = 0;
                if s.card_inserted {
                    v |= PS_CARD_INSERTED;
                }
                if s.write_switch_on {
                    v |= PS_WRITE_ENABLED;
                }
                v
            }
            CAPABILITIES => s.caps,
            msm::CORE_PWRCTL_STATUS => s.pwr_status,
            _ => s.r32(offset),
        }
    }

    fn write32(&self, offset: usize, value: u32) {
        let mut s = self.0.borrow_mut();
        s.writes.push((offset, value as u64));
        match offset {
            INT_STATUS => {
                s.int_status &= !value;
                if value & INT_RESPONSE != 0 {
                    s.int_status |= core::mem::take(&mut s.pending_int);
                }
            }
            ADMA_ADDRESS => s.adma_addr = (s.adma_addr & !0xffff_ffff) | value as u64,
            ADMA_ADDRESS_HI => s.adma_addr = (s.adma_addr & 0xffff_ffff) | (value as u64) << 32,
            msm::CORE_PWRCTL_CLEAR => s.pwr_status &= !value,
            msm::CORE_PWRCTL_CTL => {
                s.pwr_ctl_writes.push(value);
            }
            _ => s.w32(offset, value),
        }
    }

    fn read16(&self, offset: usize) -> u16 {
        let s = self.0.borrow();
        match offset {
            HOST_VERSION => s.version,
            CLOCK_CONTROL => {
                let v = u16::from_le_bytes([s.r8(offset), s.r8(offset + 1)]);
                if v & CLOCK_INT_EN != 0 {
                    v | CLOCK_INT_STABLE
                } else {
                    v
                }
            }
            _ => u16::from_le_bytes([s.r8(offset), s.r8(offset + 1)]),
        }
    }

    fn read8(&self, offset: usize) -> u8 {
        let mut s = self.0.borrow_mut();
        if offset == SOFTWARE_RESET {
            let lines = if s.lines_reset_stuck {
                s.lines_reset
            } else {
                s.lines_reset = 0;
                0
            };
            if s.reset_all_stuck && s.reset_in_progress {
                return RESET_ALL | lines;
            }
            if s.reset_in_progress {
                if s.reset_waits_for_pwr && s.pwr_status != 0 {
                    return RESET_ALL | lines;
                }
                s.reset_in_progress = false;
            }
            return lines;
        }
        s.r8(offset)
    }

    fn write16(&self, offset: usize, value: u16) {
        let mut s = self.0.borrow_mut();
        s.writes.push((offset, value as u64));
        match offset {
            COMMAND => s.run_command(value),
            _ => {
                let b = value.to_le_bytes();
                s.w8(offset, b[0]);
                s.w8(offset + 1, b[1]);
            }
        }
    }

    fn write8(&self, offset: usize, value: u8) {
        let mut s = self.0.borrow_mut();
        s.writes.push((offset, value as u64));
        match offset {
            SOFTWARE_RESET => {
                s.lines_reset = value & (RESET_CMD | RESET_DATA);
                if value & RESET_ALL != 0 {
                    s.reset_all_count += 1;
                    s.reset_in_progress = true;
                    if s.qcom {
                        s.pwr_status |= msm::PWRCTL_BUS_OFF | msm::PWRCTL_IO_LOW;
                    }
                }
            }
            POWER_CONTROL => {
                s.power_written.push(value);
                s.w8(offset, value);
                if s.qcom && value & POWER_ON != 0 {
                    s.pwr_status |= msm::PWRCTL_BUS_ON;
                }
            }
            _ => s.w8(offset, value),
        }
    }
}

/// The driver's DMA area in the model's memory, keeping count of the syncs a non-coherent device needs.
#[derive(Clone)]
pub struct TestDma {
    pub model: Model,
    pub base: u64,
    pub to_device: Rc<Cell<u32>>,
    pub from_device: Rc<Cell<u32>>,
}

impl TestDma {
    pub fn new(model: &Model, base: u64) -> Self {
        Self {
            model: model.clone(),
            base,
            to_device: Rc::new(Cell::new(0)),
            from_device: Rc::new(Cell::new(0)),
        }
    }
}

impl DmaArea for TestDma {
    fn daddr(&self) -> u64 {
        self.base
    }
    fn read(&self, offset: usize, buf: &mut [u8]) {
        let s = self.model.0.borrow();
        for (i, b) in buf.iter_mut().enumerate() {
            *b = *s.mem.get(&(self.base + (offset + i) as u64)).unwrap_or(&0);
        }
    }
    fn write(&self, offset: usize, buf: &[u8]) {
        let mut s = self.model.0.borrow_mut();
        for (i, b) in buf.iter().enumerate() {
            s.mem.insert(self.base + (offset + i) as u64, *b);
        }
    }
    fn sync_to_device(&self, _offset: usize, _len: usize) {
        self.to_device.set(self.to_device.get() + 1);
    }
    fn sync_from_device(&self, _offset: usize, _len: usize) {
        self.from_device.set(self.from_device.get() + 1);
    }
}
