//! One RAM array standing in for the three SMEM items of an edge, and a port onto it for each side.
//!
//! Flat layout (byte offsets): the 32-byte descriptor at 0, then the AP-to-remote ring (item 479), then the
//! remote-to-AP ring (item 480). A port panics on an access that is not a whole aligned word, that is outside the
//! three areas, or that writes something its side does not own: those would be bugs the real hardware would not
//! forgive.

use std::cell::RefCell;
use std::rc::Rc;
use std::vec;
use std::vec::Vec;

use crate::fifo::{Doorbell, Layout, Region, Shared, Side};

pub const DESC_LEN: usize = 32;

pub struct Ram {
    pub words: Vec<u32>,
    pub tx_len: usize,
    pub rx_len: usize,
    /// Whether the remote has "allocated" item 480 yet (what `Shared::rx_fifo` reports to the AP).
    pub rx_visible: bool,
    pub bells_to_remote: u32,
    pub bells_to_ap: u32,
    pub reads: u64,
    pub writes: u64,
}

impl Ram {
    pub fn new(tx_len: usize, rx_len: usize) -> Rc<RefCell<Ram>> {
        Rc::new(RefCell::new(Ram {
            words: vec![0; (DESC_LEN + tx_len + rx_len) / 4],
            tx_len,
            rx_len,
            rx_visible: true,
            bells_to_remote: 0,
            bells_to_ap: 0,
            reads: 0,
            writes: 0,
        }))
    }

    pub fn tx_region(&self) -> Region {
        Region {
            off: DESC_LEN,
            len: self.tx_len,
        }
    }

    pub fn rx_region(&self) -> Region {
        Region {
            off: DESC_LEN + self.tx_len,
            len: self.rx_len,
        }
    }

    /// The descriptor words as the AP names them: tx_tail, tx_head, rx_tail, rx_head.
    pub fn desc(&self, i: usize) -> u32 {
        self.words[i]
    }

    pub fn set_desc(&mut self, i: usize, v: u32) {
        self.words[i] = v;
    }

    /// Writes bytes into the AP's receive ring at byte `pos`, as the remote would.
    pub fn poke_rx(&mut self, pos: usize, bytes: &[u8]) {
        let base = self.rx_region().off;
        for (i, b) in bytes.iter().enumerate() {
            let p = base + (pos + i) % self.rx_len;
            let w = &mut self.words[p / 4];
            let mut le = w.to_le_bytes();
            le[p % 4] = *b;
            *w = u32::from_le_bytes(le);
        }
    }
}

pub struct Port {
    pub ram: Rc<RefCell<Ram>>,
    pub side: Side,
}

impl Port {
    pub fn new(ram: &Rc<RefCell<Ram>>, side: Side) -> Port {
        Port {
            ram: ram.clone(),
            side,
        }
    }

    fn check(&self, off: usize, write: bool) {
        assert!(off.is_multiple_of(4), "unaligned access at {off:#x}");
        let r = self.ram.borrow();
        let tx = r.tx_region();
        let rx = r.rx_region();
        let in_tx = off >= tx.off && off < tx.off + tx.len;
        let in_rx = off >= rx.off && off < rx.off + rx.len;
        let in_desc = off < DESC_LEN;
        assert!(
            in_tx || in_rx || in_desc,
            "access outside the three areas at {off:#x}"
        );
        if write {
            // The AP writes descriptor words 1 (tx head) and 2 (rx tail) and its tx ring; the remote 0, 3 and the rx ring.
            let ok = match self.side {
                Side::Ap => in_tx || (in_desc && matches!(off / 4, 1 | 2)),
                Side::Remote => in_rx || (in_desc && matches!(off / 4, 0 | 3)),
            };
            assert!(
                ok,
                "{:?} wrote something it does not own at {off:#x}",
                self.side
            );
        }
    }
}

impl Shared for Port {
    fn read32(&mut self, off: usize) -> u32 {
        self.check(off, false);
        let mut r = self.ram.borrow_mut();
        r.reads += 1;
        r.words[off / 4]
    }

    fn write32(&mut self, off: usize, value: u32) {
        self.check(off, true);
        let mut r = self.ram.borrow_mut();
        r.writes += 1;
        r.words[off / 4] = value;
    }

    fn rx_fifo(&mut self) -> Option<Region> {
        let r = self.ram.borrow();
        (self.side == Side::Ap && r.rx_visible).then(|| r.rx_region())
    }
}

impl Doorbell for Port {
    fn ring(&mut self) {
        let mut r = self.ram.borrow_mut();
        match self.side {
            Side::Ap => r.bells_to_remote += 1,
            Side::Remote => r.bells_to_ap += 1,
        }
    }
}

pub fn ap_layout(ram: &Rc<RefCell<Ram>>, rx_known: bool) -> Layout {
    let ram = ram.borrow();
    Layout {
        side: Side::Ap,
        desc: 0,
        tx: ram.tx_region(),
        rx: rx_known.then(|| ram.rx_region()),
    }
}

/// The modem's view of the same memory: its transmit ring is the AP's receive ring, and the other way round.
pub fn remote_layout(ram: &Rc<RefCell<Ram>>) -> Layout {
    let ram = ram.borrow();
    Layout {
        side: Side::Remote,
        desc: 0,
        tx: ram.rx_region(),
        rx: Some(ram.tx_region()),
    }
}

/// xorshift64*, for the garbage tests.
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}
