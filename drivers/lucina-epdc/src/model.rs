//! A model of the hardware for the tests: the controller's status, the SoC's registers, the PMIC and its rails, and a log
//! of what happened to the panel, in order.

use alloc::{collections::BTreeMap, rc::Rc, vec::Vec};
use core::cell::RefCell;

use crate::{
    Error, Hw,
    engine::{Buffers, Engine, Finished, Rect},
    pmic::PmicBus,
    waveform::Waveform,
};

const EPDC: usize = 0x020f_4000;
const CTRL: usize = EPDC;
const CTRL_SET: usize = EPDC + 0x004;
const CTRL_CLEAR: usize = EPDC + 0x008;
const CTRL_SFTRST: u32 = 1 << 31;
const CTRL_CLKGATE: u32 = 1 << 30;
const UPD_CORD: usize = EPDC + 0x120;
const UPD_SIZE: usize = EPDC + 0x140;
const UPD_CTRL: usize = EPDC + 0x160;
const UPD_FIXED: usize = EPDC + 0x180;
const IRQ1: usize = EPDC + 0x3e0;
const IRQ1_CLEAR: usize = EPDC + 0x3e8;
const IRQ: usize = EPDC + 0x420;
const IRQ_CLEAR: usize = EPDC + 0x428;
const STATUS_LUTS: usize = EPDC + 0x440;
const STATUS_COL: usize = EPDC + 0x480;
const STATUS: usize = EPDC + 0x4a0;
const UPD_COL_CORD: usize = EPDC + 0x4c0;
const UPD_COL_SIZE: usize = EPDC + 0x4e0;
const IRQ_WB_CMPLT: u32 = 1 << 16;
const IRQ_LUT_COL: u32 = 1 << 17;
const STATUS_UPD_VOID: u32 = 1 << 3;
const GPIO2_PSR: usize = 0x020a_0000 + 0x08;
const POWER_GOOD: u32 = 1 << 13;
const CCGR3: usize = 0x020c_4000 + 0x74;
const GPC_CNTR: usize = 0x020d_c000;
const PAD_D0_MUX: usize = 0x020e_0000 + 0x090;
const ALT_EPDC: u32 = 0;

const LSW4: u8 = 0x5f;
const EPDCNFG: u8 = 0x60;
const EPDINTS: u8 = 0x61;
const ADCCNTL: u8 = 0x26;
const ADCICNFG: u8 = 0x30;
const ADCDATAL: u8 = 0x2b;
const ADCDATAH: u8 = 0x2c;

/// What happens to the panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Logic(bool),
    HighVoltages(bool),
    Vcom(bool),
    Pads(bool),
    Clocks(bool),
    UpdateStarted,
    PmicVoltageWrite,
    ControllerReset,
}

#[derive(Default)]
struct Shared {
    regs: BTreeMap<usize, u32>,
    pmic: BTreeMap<u8, u8>,
    pmic_writes: Vec<(u8, u8)>,
    log: Vec<Event>,
    /// The log's length when the INIT ended, for tests about later updates.
    no_power_good: bool,
    fault: u8,
    stuck_adc: bool,
    adc_codes: (u16, u16),
    adc_reads: u32,
    in_wb: bool,
    last_lut: usize,
    max_lut: usize,
    down_with_lut_active: bool,
    fail_writes: u32,
}

/// The SoC's side.
#[derive(Clone, Default)]
pub struct Model {
    shared: Rc<RefCell<Shared>>,
    pub stuck_power_domain: bool,
}

impl Model {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, addr: usize) -> u32 {
        self.shared.borrow().regs.get(&addr).copied().unwrap_or(0)
    }

    pub fn set(&mut self, addr: usize, val: u32) {
        self.shared.borrow_mut().regs.insert(addr, val);
    }
}

impl Hw for Model {
    fn read32(&mut self, addr: usize) -> u32 {
        let shared = self.shared.borrow();
        let val = shared.regs.get(&addr).copied().unwrap_or(0);
        if addr == GPIO2_PSR {
            let on = shared.pmic.get(&EPDCNFG).copied().unwrap_or(0) & 0x80 != 0;
            return if on && !shared.no_power_good {
                POWER_GOOD
            } else {
                0
            };
        }
        if addr == GPC_CNTR && !self.stuck_power_domain {
            return val & !(1 << 5);
        }
        val
    }

    fn write32(&mut self, addr: usize, val: u32) {
        let mut shared = self.shared.borrow_mut();
        let old = shared.regs.get(&addr).copied().unwrap_or(0);
        let mut store = val;
        match addr {
            CTRL_SET => {
                let mut ctrl = shared.regs.get(&CTRL).copied().unwrap_or(0) | val;
                if val & CTRL_SFTRST != 0 {
                    ctrl |= CTRL_CLKGATE;
                    shared.log.push(Event::ControllerReset);
                }
                shared.regs.insert(CTRL, ctrl);
                return;
            }
            CTRL_CLEAR => {
                let ctrl = shared.regs.get(&CTRL).copied().unwrap_or(0) & !val;
                shared.regs.insert(CTRL, ctrl);
                return;
            }
            IRQ_CLEAR => {
                let irq = shared.regs.get(&IRQ).copied().unwrap_or(0) & !val;
                shared.regs.insert(IRQ, irq);
                return;
            }
            IRQ1_CLEAR => {
                let irq = shared.regs.get(&IRQ1).copied().unwrap_or(0) & !val;
                shared.regs.insert(IRQ1, irq);
                return;
            }
            UPD_CTRL => {
                let lut = ((val >> 16) & 0x3f) as usize;
                shared.last_lut = lut;
                shared.max_lut = shared.max_lut.max(lut);
                shared.in_wb = true;
                let luts = shared.regs.get(&STATUS_LUTS).copied().unwrap_or(0) | (1 << lut);
                shared.regs.insert(STATUS_LUTS, luts);
                shared.log.push(Event::UpdateStarted);
            }
            CCGR3 => {
                let was = old & (3 << 4) != 0;
                let is = val & (3 << 4) != 0;
                if was != is {
                    shared.log.push(Event::Clocks(is));
                }
                store = val;
            }
            PAD_D0_MUX => {
                shared.log.push(Event::Pads(val == ALT_EPDC));
            }
            _ => {}
        }
        shared.regs.insert(addr, store);
    }

    fn delay_us(&mut self, _us: u32) {}

    fn barrier(&mut self) {}
}

/// The PMIC's side.
#[derive(Clone, Default)]
pub struct FakePmic {
    shared: Rc<RefCell<Shared>>,
    writes: Rc<RefCell<Vec<(u8, u8)>>>,
}

impl FakePmic {
    pub fn get(&self, reg: u8) -> u8 {
        self.shared.borrow().pmic.get(&reg).copied().unwrap_or(0)
    }

    pub fn set(&mut self, reg: u8, val: u8) {
        self.shared.borrow_mut().pmic.insert(reg, val);
    }

    pub fn writes(&self) -> Rc<RefCell<Vec<(u8, u8)>>> {
        self.writes.clone()
    }

    pub fn adc_codes(&mut self, ain0: u16, ain2: u16) {
        self.shared.borrow_mut().adc_codes = (ain0, ain2);
    }

    pub fn stuck_adc(&mut self) {
        self.shared.borrow_mut().stuck_adc = true;
    }
}

impl PmicBus for FakePmic {
    fn read(&mut self, reg: u8) -> Result<u8, Error> {
        let shared = self.shared.borrow();
        Ok(match reg {
            EPDINTS => {
                let on = shared.pmic.get(&EPDCNFG).copied().unwrap_or(0) & 0x80 != 0;
                shared.pmic.get(&EPDINTS).copied().unwrap_or(0) | if on { shared.fault } else { 0 }
            }
            ADCDATAL | ADCDATAH => {
                let mux = shared.pmic.get(&ADCICNFG).copied().unwrap_or(0) >> 2;
                let code = if mux == 0 {
                    shared.adc_codes.0
                } else {
                    shared.adc_codes.1
                };
                if reg == ADCDATAL {
                    code as u8
                } else {
                    (code >> 8) as u8
                }
            }
            _ => shared.pmic.get(&reg).copied().unwrap_or(0),
        })
    }

    fn write(&mut self, reg: u8, val: u8) -> Result<(), Error> {
        let mut shared = self.shared.borrow_mut();
        if shared.fail_writes > 0 {
            shared.fail_writes -= 1;
            return Err(Error::I2cTimeout);
        }
        let old = shared.pmic.get(&reg).copied().unwrap_or(0);
        self.writes.borrow_mut().push((reg, val));
        shared.pmic_writes.push((reg, val));
        let mut store = val;
        match reg {
            LSW4 if (old ^ val) & 1 != 0 => shared.log.push(Event::Logic(val & 1 != 0)),
            EPDCNFG => {
                if (old ^ val) & 0x80 != 0 {
                    if val & 0x80 == 0 && shared.regs.get(&STATUS_LUTS).copied().unwrap_or(0) != 0 {
                        shared.down_with_lut_active = true;
                    }
                    shared.log.push(Event::HighVoltages(val & 0x80 != 0));
                }
                if (old ^ val) & 0x40 != 0 {
                    shared.log.push(Event::Vcom(val & 0x40 != 0));
                }
            }
            0x64 | 0x68 | 0xb4 => shared.log.push(Event::PmicVoltageWrite),
            ADCCNTL if val & 0x10 != 0 => {
                shared.adc_reads += 1;
                if !shared.stuck_adc {
                    store = val & !0x10;
                }
            }
            _ => {}
        }
        shared.pmic.insert(reg, store);
        Ok(())
    }

    fn delay_us(&mut self, _us: u32) {}
}

/// The whole panel, with an engine on it.
pub struct Panel {
    pub engine: Engine<Model, FakePmic>,
    hw: Model,
    bus: FakePmic,
    finished: Vec<Finished>,
    init_log_end: usize,
}

impl Panel {
    pub const UPDATE_BUFFER: u32 = 0x8100_0000;
    pub const WORKING_BUFFER: u32 = 0x8200_0000;
    pub const WAVEFORM_BUFFER: u32 = 0x8300_0000;

    pub fn new() -> Self {
        Self::build(false)
    }

    /// With the rails on, as a kernel that stopped in the middle of an update leaves them.
    pub fn started_with_rails_on() -> Self {
        Self::build(true)
    }

    pub fn fail_pmic_writes(&mut self, count: u32) {
        self.hw.shared.borrow_mut().fail_writes = count;
    }

    fn build(rails_on: bool) -> Self {
        let shared = Rc::new(RefCell::new(Shared::default()));
        let hw = Model {
            shared: shared.clone(),
            stuck_power_domain: false,
        };
        let mut bus = FakePmic {
            shared,
            writes: Rc::default(),
        };
        bus.set(0x64, 244);
        bus.set(0x68, 21);
        bus.adc_codes(0x2a0, 0x00b);
        if rails_on {
            bus.set(EPDCNFG, 0xc0);
            bus.set(LSW4, 0x01);
        }
        let buffers = Buffers {
            update: Self::UPDATE_BUFFER,
            working: Self::WORKING_BUFFER,
            waveform: Self::WAVEFORM_BUFFER,
        };
        let mut file = alloc::vec![0u8; 200];
        file[16] = 25;
        file[36] = 4;
        file[38] = 13;
        file[48..62].copy_from_slice(&[0, 3, 6, 9, 12, 15, 18, 21, 24, 27, 30, 33, 38, 43]);
        let waveform = Waveform::parse(&file).unwrap();
        let engine = Engine::new(hw.clone(), bus.clone(), buffers, waveform).unwrap();
        if !rails_on {
            hw.shared.borrow_mut().log.clear();
        }
        Self {
            engine,
            hw,
            bus,
            finished: Vec::new(),
            init_log_end: 0,
        }
    }

    pub fn hw(&self) -> Model {
        self.hw.clone()
    }

    pub fn bus(&self) -> FakePmic {
        self.bus.clone()
    }

    pub fn pmic_writes(&self) -> usize {
        self.hw.shared.borrow().pmic_writes.len()
    }

    pub fn service(&mut self, now_ns: u64) {
        self.engine.service(now_ns);
        let finished: Vec<_> = self.engine.take_finished().collect();
        self.finished.extend(finished);
        if self.init_log_end == 0 && !self.finished.is_empty() {
            self.init_log_end = self.hw.shared.borrow().log.len();
        }
    }

    pub fn finished(&self) -> Vec<Finished> {
        self.finished.clone()
    }

    pub fn get(&self, addr: usize) -> u32 {
        self.hw.get(addr)
    }

    pub fn pmic(&self, reg: u8) -> u8 {
        self.bus.get(reg)
    }

    pub fn set_pmic(&mut self, reg: u8, val: u8) {
        self.bus.set(reg, val);
    }

    pub fn last_lut(&self) -> usize {
        self.hw.shared.borrow().last_lut
    }

    pub fn max_lut_used(&self) -> usize {
        self.hw.shared.borrow().max_lut
    }

    pub fn in_wb(&self) -> bool {
        self.hw.shared.borrow().in_wb
    }

    pub fn active_luts(&self) -> u32 {
        self.get(STATUS_LUTS).count_ones()
    }

    pub fn adc_reads(&self) -> u32 {
        self.hw.shared.borrow().adc_reads
    }

    pub fn set_temperature_codes(&mut self, ain0: u16, ain2: u16) {
        self.bus.adc_codes(ain0, ain2);
    }

    pub fn stuck_adc(&mut self) {
        self.bus.stuck_adc();
    }

    pub fn fault_on_high_voltages(&mut self, bits: u8) {
        self.hw.shared.borrow_mut().fault = bits;
    }

    pub fn no_power_good(&mut self) {
        self.hw.shared.borrow_mut().no_power_good = true;
    }

    /// The working-buffer pass of the last update ends.
    pub fn complete_working_buffer(&mut self, collision: bool) {
        let mut irq = self.get(IRQ) | IRQ_WB_CMPLT;
        if collision {
            irq |= IRQ_LUT_COL;
        }
        self.hw.set(IRQ, irq);
        self.hw.shared.borrow_mut().in_wb = false;
    }

    /// Says that the last update has nothing to draw.
    pub fn void_update(&mut self) {
        self.hw.set(STATUS, self.get(STATUS) | STATUS_UPD_VOID);
        let lut = self.last_lut();
        self.hw
            .set(STATUS_LUTS, self.get(STATUS_LUTS) & !(1 << lut));
    }

    pub fn collide_with(&mut self, lut: usize, rect: Rect) {
        self.hw.set(STATUS_COL, 1 << lut);
        self.hw.set(UPD_COL_CORD, (rect.y << 16) | rect.x);
        self.hw.set(UPD_COL_SIZE, (rect.h << 16) | rect.w);
    }

    /// A LUT's scan ends.
    pub fn finish_lut(&mut self, lut: usize) {
        self.hw.set(IRQ1, self.get(IRQ1) | (1 << lut));
        self.hw
            .set(STATUS_LUTS, self.get(STATUS_LUTS) & !(1 << lut));
        self.hw.set(STATUS, self.get(STATUS) & !STATUS_UPD_VOID);
    }

    pub fn fixed_pixel(&self) -> Option<u8> {
        let fixed = self.get(UPD_FIXED);
        (fixed & (1 << 31) != 0).then_some((fixed >> 8) as u8)
    }

    pub fn started_mode(&self) -> u32 {
        (self.get(UPD_CTRL) >> 8) & 0xff
    }

    pub fn started_full(&self) -> bool {
        self.get(UPD_CTRL) & 1 != 0
    }

    pub fn started_rect(&self) -> Rect {
        let cord = self.get(UPD_CORD);
        let size = self.get(UPD_SIZE);
        Rect {
            x: cord & 0xffff,
            y: cord >> 16,
            w: size & 0xffff,
            h: size >> 16,
        }
    }

    pub fn has_event(&self, event: Event) -> bool {
        self.hw.shared.borrow().log.contains(&event)
    }

    /// Whether this happened after the first update ended.
    pub fn has_event_after_init(&self, event: Event) -> bool {
        self.hw.shared.borrow().log[self.init_log_end..].contains(&event)
    }

    /// These events happened, in this order, with others between them.
    pub fn assert_order(&self, expected: &[Event]) {
        let shared = self.hw.shared.borrow();
        let mut log = shared.log.iter();
        for event in expected {
            assert!(
                log.any(|e| e == event),
                "{event:?} out of order in {:?}",
                shared.log
            );
        }
    }

    pub fn assert_never_down_with_a_lut_active(&self) {
        assert!(!self.hw.shared.borrow().down_with_lut_active);
    }

    /// Everything is off, and the rails came down in the right order.
    pub fn assert_all_off(&self) {
        assert_eq!(self.pmic(EPDCNFG) & 0xc0, 0);
        assert_eq!(self.pmic(LSW4) & 1, 0);
        assert_eq!(self.get(CCGR3) & (3 << 4), 0);
        assert_ne!(self.get(PAD_D0_MUX), ALT_EPDC);
    }

    /// The voltages were written before anything went up.
    pub fn assert_voltages_before_rails(&self) {
        let shared = self.hw.shared.borrow();
        let last_voltage = shared
            .log
            .iter()
            .rposition(|event| *event == Event::PmicVoltageWrite)
            .unwrap();
        let first_rail = shared
            .log
            .iter()
            .position(|event| matches!(event, Event::Logic(true) | Event::HighVoltages(true)))
            .unwrap();
        assert!(last_voltage < first_rail);
    }
}
