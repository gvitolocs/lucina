//! A software model of the hardware for the tests: registers with their set, clear and write-1-to-clear behaviour, the
//! I2C controller and PMIC, and the host side of the USB port, which walks the same queue heads and transfer descriptors
//! as the controller does.

use alloc::{rc::Rc, vec, vec::Vec};
use core::cell::RefCell;
use std::collections::HashMap;

use crate::{
    DmaMem, Hw,
    dcd::{DMA_SIZE, Ep},
};

const USB: usize = 0x0218_4000;
const USBCMD: usize = USB + 0x140;
const USBSTS: usize = USB + 0x144;
const DEVICEADDR: usize = USB + 0x154;
const ENDPTLISTADDR: usize = USB + 0x158;
const PORTSC: usize = USB + 0x184;
const ENDPTSETUPSTAT: usize = USB + 0x1ac;
const ENDPTPRIME: usize = USB + 0x1b0;
const ENDPTFLUSH: usize = USB + 0x1b4;
const ENDPTSTAT: usize = USB + 0x1b8;
const ENDPTCOMPLETE: usize = USB + 0x1bc;
const ENDPTCTRL0: usize = USB + 0x1c0;
const USBMISC: usize = 0x0218_4800;

const CCGR2: usize = 0x020c_4070;
const CCGR6: usize = 0x020c_4080;
const PLL3: usize = 0x020c_8010;
const PHY_CTRL: usize = 0x020c_9030;

const I2C_IFDR: usize = 0x021a_0004;
const I2C_I2CR: usize = 0x021a_0008;
const I2C_I2SR: usize = 0x021a_000c;
const I2C_I2DR: usize = 0x021a_0010;

const STS_UI: u32 = 1;
const STS_PCI: u32 = 1 << 2;
const STS_URI: u32 = 1 << 6;
const PORTSC_HSP: u32 = 1 << 9;

const DMA_BASE: u32 = 0x8010_0000;

const fn ep_bit(ep: Ep) -> u32 {
    (1 << ep.num) << if ep.is_in { 16 } else { 0 }
}

const fn qh(ep: Ep) -> usize {
    (ep.num as usize * 2 + ep.is_in as usize) * 64
}

/// An access, in the order they happened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Write32(usize, u32),
    PmicWrite(u8, u8),
}

/// The I2C transaction in progress.
#[derive(Clone, Copy, Debug, PartialEq)]
enum I2cState {
    Idle,
    Address,
    Register,
    Value(u8),
    ReadAddress(u8),
    ReadData(u8),
}

/// The memory the controller uses, shared with the model of the controller.
pub struct ModelMem(Rc<RefCell<Vec<u8>>>);

impl DmaMem for ModelMem {
    fn paddr(&self) -> u32 {
        DMA_BASE
    }

    fn read32(&self, offset: usize) -> u32 {
        let mem = self.0.borrow();
        u32::from_le_bytes(mem[offset..offset + 4].try_into().unwrap())
    }

    fn write32(&mut self, offset: usize, val: u32) {
        self.0.borrow_mut()[offset..offset + 4].copy_from_slice(&val.to_le_bytes());
    }

    fn read(&self, offset: usize, out: &mut [u8]) {
        out.copy_from_slice(&self.0.borrow()[offset..offset + out.len()]);
    }

    fn write(&mut self, offset: usize, data: &[u8]) {
        self.0.borrow_mut()[offset..offset + data.len()].copy_from_slice(data);
    }
}

pub struct Model {
    regs: HashMap<usize, u32>,
    pub accesses: Vec<Access>,
    mem: Rc<RefCell<Vec<u8>>>,

    /// Panics on an access to a block whose clock is off, as the bus would hang.
    pub check_gates: bool,
    pub pll_locks: bool,
    pub phy_stuck: bool,

    pub i2c_ifdr: u16,
    i2cr: u16,
    i2sr: u16,
    i2c_state: I2cState,
    i2c_reg: u8,
    i2c_read: u16,
    pub pmic: [u8; 256],
    pub pmic_writes: Vec<(u8, u8)>,
    pub pmic_nak: bool,

    /// A packet that lands while the driver copies a setup packet: it clears the tripwire.
    pub trip_once: Option<[u8; 8]>,
    pub status_acks: Vec<u32>,
    pub last_status: u32,
}

impl Model {
    pub fn new() -> Self {
        Self {
            regs: HashMap::new(),
            accesses: Vec::new(),
            mem: Rc::new(RefCell::new(vec![0; DMA_SIZE])),
            check_gates: false,
            pll_locks: true,
            phy_stuck: false,
            i2c_ifdr: 0x2b,
            i2cr: 0,
            i2sr: 0,
            i2c_state: I2cState::Idle,
            i2c_reg: 0,
            i2c_read: 0,
            pmic: [0; 256],
            pmic_writes: Vec::new(),
            pmic_nak: false,
            trip_once: None,
            status_acks: Vec::new(),
            last_status: 0,
        }
    }

    pub fn mem(&self, size: usize) -> ModelMem {
        assert_eq!(size, DMA_SIZE);
        ModelMem(self.mem.clone())
    }

    pub fn set(&mut self, addr: usize, val: u32) {
        self.regs.insert(addr, val);
    }

    pub fn get(&self, addr: usize) -> u32 {
        self.regs.get(&addr).copied().unwrap_or(0)
    }

    pub fn writes32(&self) -> Vec<(usize, u32)> {
        self.accesses
            .iter()
            .filter_map(|access| match access {
                Access::Write32(addr, val) => Some((*addr, *val)),
                Access::PmicWrite(..) => None,
            })
            .collect()
    }

    pub fn access_index(&self, access: Access) -> usize {
        self.accesses
            .iter()
            .position(|a| *a == access)
            .unwrap_or_else(|| panic!("no {access:?}"))
    }

    fn check_clock(&self, addr: usize) {
        if !self.check_gates {
            return;
        }
        if (USB..USB + 0x1000).contains(&addr) {
            assert_eq!(
                self.get(CCGR6) & 3,
                3,
                "USB access at {addr:#x} with its clock gated"
            );
        }
        if (0x020c_9000..0x020c_a000).contains(&addr) {
            assert_eq!(
                self.get(PORTSC) & (1 << 23),
                0,
                "PHY access at {addr:#x} with its clock stopped"
            );
        }
        if (0x021a_0000..0x021a_4000).contains(&addr) {
            assert_eq!(
                self.get(CCGR2) & (3 << 6),
                3 << 6,
                "I2C access at {addr:#x} with its clock gated"
            );
        }
    }

    // --- The host's side ---

    pub fn host_bus_reset(&mut self, high_speed: bool) {
        self.set(PORTSC, if high_speed { PORTSC_HSP } else { 0 });
        self.raise(STS_URI | STS_PCI);
        self.regs.remove(&ENDPTSTAT);
    }

    fn raise(&mut self, bits: u32) {
        let status = self.get(USBSTS) | bits;
        self.set(USBSTS, status);
        self.last_status = status;
    }

    pub fn host_setup(&mut self, mem: &mut ModelMem, setup: [u8; 8]) {
        mem.write(qh(Ep::new(0)) + 0x28, &setup);
        self.set(ENDPTSETUPSTAT, 1);
        self.raise(STS_UI);
    }

    pub fn is_primed(&self, ep: Ep) -> bool {
        self.get(ENDPTSTAT) & ep_bit(ep) != 0
    }

    pub fn has_second_td(&self, mem: &ModelMem, ep: Ep) -> bool {
        let first = (mem.read32(qh(ep) + 0x08) - DMA_BASE) as usize;
        mem.read32(first) & 1 == 0
    }

    pub fn qh_max_packet(&self, mem: &ModelMem, ep: Ep) -> u32 {
        (mem.read32(qh(ep)) >> 16) & 0x7ff
    }

    fn walk(&mut self, mem: &mut ModelMem, ep: Ep, data: Option<&[u8]>) -> Vec<u8> {
        assert!(self.is_primed(ep), "{ep:?} is not primed");
        let mut td = (mem.read32(qh(ep) + 0x08) - DMA_BASE) as usize;
        let mut out = Vec::new();
        loop {
            let token = mem.read32(td + 4);
            assert_ne!(token & (1 << 7), 0, "descriptor not active");
            let total = ((token >> 16) & 0x7fff) as usize;
            let buffer = (mem.read32(td + 8) - DMA_BASE) as usize;
            let remaining = match data {
                Some(data) => {
                    assert!(data.len() <= total, "more data than the descriptor takes");
                    mem.write(buffer, data);
                    total - data.len()
                }
                None => {
                    let mut bytes = vec![0; total];
                    mem.read(buffer, &mut bytes);
                    out.extend_from_slice(&bytes);
                    0
                }
            };
            mem.write32(
                td + 4,
                (token & !(1 << 7) & !(0x7fff << 16)) | ((remaining as u32) << 16),
            );
            let next = mem.read32(td);
            if next & 1 != 0 {
                assert_ne!(token & (1 << 15), 0, "the last descriptor must interrupt");
                break;
            }
            td = (next - DMA_BASE) as usize;
        }
        let stat = self.get(ENDPTSTAT) & !ep_bit(ep);
        self.set(ENDPTSTAT, stat);
        let complete = self.get(ENDPTCOMPLETE) | ep_bit(ep);
        self.set(ENDPTCOMPLETE, complete);
        self.raise(STS_UI);
        out
    }

    /// The host reads what the device has ready on this endpoint.
    pub fn host_in(&mut self, mem: &mut ModelMem, ep: Ep) -> Vec<u8> {
        self.walk(mem, ep, None)
    }

    /// The host sends this to the endpoint.
    pub fn host_out(&mut self, mem: &mut ModelMem, ep: Ep, data: &[u8]) {
        self.walk(mem, ep, Some(data));
    }

    pub fn flushes_of(&self, bits: u32) -> usize {
        self.accesses
            .iter()
            .filter(|a| **a == Access::Write32(ENDPTFLUSH, bits))
            .count()
    }

    pub fn address(&self) -> u32 {
        self.get(DEVICEADDR) >> 25
    }

    pub fn list_address(&self) -> u32 {
        self.get(ENDPTLISTADDR)
    }

    pub fn ep0_stalled(&self) -> bool {
        self.get(ENDPTCTRL0) & 0x0001_0001 == 0x0001_0001
    }

    // --- I2C ---

    fn i2c_write_cr(&mut self, val: u16) {
        let starting = val & (1 << 5) != 0 && self.i2cr & (1 << 5) == 0;
        let restarting = val & (1 << 2) != 0;
        let stopping = val & (1 << 5) == 0 && self.i2cr & (1 << 5) != 0;
        self.i2cr = val;
        if starting {
            self.i2sr |= 1 << 5;
            self.i2c_state = I2cState::Address;
        }
        if restarting {
            self.i2c_state = I2cState::ReadAddress(self.i2c_reg);
        }
        if stopping || val & (1 << 7) == 0 {
            self.i2sr &= !(1 << 5);
            self.i2c_state = I2cState::Idle;
        }
    }

    fn i2c_write_dr(&mut self, byte: u8) {
        let nak = |model: &mut Self| model.i2sr |= 1;
        self.i2sr &= !1;
        self.i2sr |= 1 << 1;
        match self.i2c_state {
            I2cState::Address => {
                if self.pmic_nak || byte != 0x3c << 1 {
                    nak(self);
                }
                self.i2c_state = I2cState::Register;
            }
            I2cState::Register => {
                self.i2c_reg = byte;
                self.i2c_state = I2cState::Value(byte);
            }
            I2cState::Value(reg) => {
                self.pmic[reg as usize] = byte;
                self.pmic_writes.push((reg, byte));
                self.accesses.push(Access::PmicWrite(reg, byte));
            }
            I2cState::ReadAddress(reg) => {
                assert_eq!(byte, (0x3c << 1) | 1);
                self.i2c_state = I2cState::ReadData(reg);
            }
            I2cState::ReadData(_) | I2cState::Idle => panic!("I2C write in the wrong state"),
        }
    }

    fn i2c_read_dr(&mut self) -> u16 {
        if let I2cState::ReadData(reg) = self.i2c_state {
            // The dummy read starts the byte, which is then there to read.
            self.i2c_read = self.pmic[reg as usize].into();
            self.i2sr |= 1 << 1;
        }
        self.i2c_read
    }
}

impl Hw for Model {
    fn read32(&mut self, addr: usize) -> u32 {
        self.check_clock(addr);
        match addr {
            PLL3 => (self.get(PLL3) & !(1 << 31)) | if self.pll_locks { 1 << 31 } else { 0 },
            USBCMD => {
                let cmd = self.get(USBCMD);
                // A packet that lands in the copy clears the tripwire and overwrites the buffer.
                if cmd & (1 << 13) != 0
                    && let Some(setup) = self.trip_once.take()
                {
                    let mut mem = ModelMem(self.mem.clone());
                    mem.write(0x28, &setup);
                    self.set(USBCMD, cmd & !(1 << 13));
                    return cmd & !(1 << 13);
                }
                cmd
            }
            _ => self.get(addr),
        }
    }

    fn write32(&mut self, addr: usize, val: u32) {
        self.check_clock(addr);
        self.accesses.push(Access::Write32(addr, val));
        match addr {
            // The PHY's register aliases.
            0x020c_9034 => {
                let new = self.get(PHY_CTRL) | val;
                // As the real PHY after a kexec: the reset does not set the clock gate here.
                let new = if self.phy_stuck { new | (1 << 31) } else { new };
                self.set(PHY_CTRL, new);
            }
            0x020c_9038 => {
                if !self.phy_stuck {
                    let new = self.get(PHY_CTRL) & !val;
                    self.set(PHY_CTRL, new);
                }
            }
            // The PLL's.
            0x020c_8014 => {
                let new = self.get(PLL3) | val;
                self.set(PLL3, new);
            }
            0x020c_8018 => {
                let new = self.get(PLL3) & !val;
                self.set(PLL3, new);
            }
            // Self-clearing reset.
            USBCMD => self.set(USBCMD, val & !(1 << 1)),
            USBSTS => {
                let status = self.get(USBSTS) & !val;
                self.status_acks.push(val);
                self.set(USBSTS, status);
            }
            ENDPTSETUPSTAT | ENDPTCOMPLETE => {
                let cleared = self.get(addr) & !val;
                self.set(addr, cleared);
            }
            // Priming takes effect at once; the status shows it, and the priming bit clears.
            ENDPTPRIME => {
                let stat = self.get(ENDPTSTAT) | val;
                self.set(ENDPTSTAT, stat);
            }
            ENDPTFLUSH => {
                let stat = self.get(ENDPTSTAT) & !val;
                self.set(ENDPTSTAT, stat);
            }
            _ => self.set(addr, val),
        }
    }

    fn read16(&mut self, addr: usize) -> u16 {
        self.check_clock(addr);
        match addr {
            I2C_IFDR => self.i2c_ifdr,
            I2C_I2CR => self.i2cr,
            I2C_I2SR => self.i2sr,
            I2C_I2DR => self.i2c_read_dr(),
            _ => panic!("16-bit read of {addr:#x}"),
        }
    }

    fn write16(&mut self, addr: usize, val: u16) {
        self.check_clock(addr);
        match addr {
            I2C_IFDR => self.i2c_ifdr = val,
            I2C_I2CR => self.i2c_write_cr(val),
            I2C_I2SR => self.i2sr = (self.i2sr & ((1 << 5) | 1)) | (val & !((1 << 5) | 1)),
            I2C_I2DR => self.i2c_write_dr(val as u8),
            _ => panic!("16-bit write of {addr:#x}"),
        }
    }

    fn delay_us(&mut self, _us: u32) {}

    fn barrier(&mut self) {}
}

// Keeps the block addresses of the tests in one place for those that assert on them.
const _: usize = USBMISC;
