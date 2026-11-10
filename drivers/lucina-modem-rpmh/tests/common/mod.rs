//! Shared pieces of the tests: a recording register file, delays, the stand-in command DB image, the RSC model and a
//! reader for the device tree excerpt.
#![allow(dead_code)]

pub mod db;
pub mod dts;
pub mod sim;

use std::cell::{Cell, RefCell};

use lucina_qcom_soc::io::{Delay, RegIo};

/// A plain register file in RAM that logs every write.
pub struct Ram {
    mem: RefCell<Vec<u32>>,
    writes: RefCell<Vec<(u32, u32)>>,
    reads: Cell<u64>,
}

impl Ram {
    pub fn new(bytes: usize) -> Self {
        Ram {
            mem: RefCell::new(vec![0; bytes / 4]),
            writes: RefCell::new(Vec::new()),
            reads: Cell::new(0),
        }
    }
    /// Set a word without logging a write (as the C oracle's `poke`).
    pub fn poke(&self, off: u32, val: u32) {
        self.mem.borrow_mut()[off as usize / 4] = val;
    }
    pub fn peek(&self, off: u32) -> u32 {
        self.mem.borrow()[off as usize / 4]
    }
    pub fn writes(&self) -> Vec<(u32, u32)> {
        self.writes.borrow().clone()
    }
    pub fn clear_writes(&self) {
        self.writes.borrow_mut().clear();
    }
    pub fn reads(&self) -> u64 {
        self.reads.get()
    }
}

impl RegIo for Ram {
    fn read32(&self, off: u32) -> u32 {
        assert_eq!(off & 3, 0, "unaligned read {off:#x}");
        self.reads.set(self.reads.get() + 1);
        self.mem.borrow()[off as usize / 4]
    }
    fn write32(&self, off: u32, value: u32) {
        assert_eq!(off & 3, 0, "unaligned write {off:#x}");
        self.writes.borrow_mut().push((off, value));
        self.mem.borrow_mut()[off as usize / 4] = value;
    }
}

/// A `Delay` that only counts.
#[derive(Default)]
pub struct CountDelay(pub Cell<u32>);

impl Delay for CountDelay {
    fn sleep_ms(&self, ms: u32) {
        self.0.set(self.0.get() + ms);
    }
}

/// The `RSC_DRV_ID` and `DRV_PRNT_CHLD_CONFIG` of a DRV-2 with 8 TCSes of 3 commands: version 2.7.
pub const ID_2_7: u32 = 0x0002_0700;
pub const CONFIG_8_TCS_3_CMDS: u32 = (8 << 12) | (3 << 27);

pub fn parse_num(s: &str) -> u32 {
    match s.strip_prefix("0x") {
        Some(h) => u32::from_str_radix(h, 16).unwrap(),
        None => s.parse().unwrap(),
    }
}

/// `key=value` lookup in a fixture line.
pub fn field<'a>(line: &'a str, key: &str) -> &'a str {
    line.split_whitespace()
        .find_map(|w| w.strip_prefix(key).and_then(|r| r.strip_prefix('=')))
        .unwrap_or_else(|| panic!("no {key}= in {line}"))
}
