// SPDX-License-Identifier: GPL-2.0-only
// Derived from drivers/tty/serial/samsung_tty.c (SPDX GPL-2.0; Ben Dooks, Copyright (c) 2003-2008 Simtec Electronics) and
// include/linux/serial_s3c.h (SPDX GPL-2.0+; Copyright (C) 2002 Shane Nay, additional defines Copyright 2003 Simtec
// Electronics) of AsahiLinux/linux (branch asahi, rev 6ba0637d1ed9). Re-expressed in Rust for LucinaOS; see the crate README.
// This is a Rust port of the behaviour of drivers/tty/serial/samsung_tty.c and include/linux/serial_s3c.h; the original is the source of truth.
// Test-only software model of the S5L UART register file, written from the register definitions of serial_s3c.h and
// samsung_tty.c (asahi, rev 6ba0637d1ed9). It models only what the driver logic observes.

use crate::mmio::Mmio;
use crate::regs::*;
use core::cell::RefCell;
use std::collections::VecDeque;
use std::vec::Vec;

/// A software UART: a 16-entry transmit FIFO that the test drains, a 16-entry receive FIFO that the test fills, and plain
/// storage for ULCON, UCON, UFCON and the flag bits of UTRSTAT.
pub struct SimUart {
    pub ulcon: u32,
    pub ucon: u32,
    pub ufcon: u32,
    /// The Apple flags (bits 3..=9) latched in UTRSTAT; written 1 to clear.
    pub flags: u32,
    pub tx_fifo: VecDeque<u8>,
    pub rx_fifo: RefCell<VecDeque<u8>>,
    /// Everything the "wire" has taken from the transmit FIFO.
    pub wire: Vec<u8>,
    /// The transmitter shift register is busy (`UTRSTAT.TXE` clear).
    pub shifting: bool,
    /// Every write: (offset, value).
    pub writes: Vec<(usize, u32)>,
    pub reads: RefCell<Vec<usize>>,
}

impl SimUart {
    pub fn new() -> Self {
        Self {
            ulcon: 0,
            ucon: 0,
            ufcon: 0,
            flags: 0,
            tx_fifo: VecDeque::new(),
            rx_fifo: RefCell::new(VecDeque::new()),
            wire: Vec::new(),
            shifting: false,
            writes: Vec::new(),
            reads: RefCell::new(Vec::new()),
        }
    }

    /// Moves `n` bytes from the transmit FIFO to the wire.
    pub fn drain(&mut self, n: usize) {
        for _ in 0..n {
            if let Some(b) = self.tx_fifo.pop_front() {
                self.wire.push(b);
            }
        }
    }

    fn ufstat(&self) -> u32 {
        let tx = self.tx_fifo.len() as u32;
        let rx = self.rx_fifo.borrow().len() as u32;
        let mut v = (tx << UFSTAT_TXSHIFT) & UFSTAT_TXMASK | (rx << UFSTAT_RXSHIFT) & UFSTAT_RXMASK;
        if tx >= FIFO_SIZE {
            v |= UFSTAT_TXFULL;
            v &= !UFSTAT_TXMASK; // a full FIFO reads the count field as 0, flagged by TXFULL
        }
        if rx >= FIFO_SIZE {
            v |= UFSTAT_RXFULL;
            v &= !UFSTAT_RXMASK;
        }
        v
    }

    fn utrstat(&self) -> u32 {
        let mut v = self.flags & UTRSTAT_ALL_FLAGS;
        if !self.rx_fifo.borrow().is_empty() {
            v |= UTRSTAT_RXDR;
        }
        if self.tx_fifo.is_empty() {
            v |= UTRSTAT_TXFE;
            if !self.shifting {
                v |= UTRSTAT_TXE;
            }
        }
        v
    }
}

impl Mmio for SimUart {
    fn read32(&self, off: usize) -> u32 {
        self.reads.borrow_mut().push(off);
        match off {
            ULCON => self.ulcon,
            UCON => self.ucon,
            UFCON => self.ufcon,
            UTRSTAT => self.utrstat(),
            UFSTAT => self.ufstat(),
            // Reading URXH takes the byte out of the FIFO.
            URXH => self.rx_fifo.borrow_mut().pop_front().unwrap_or(0) as u32,
            _ => 0,
        }
    }

    fn write32(&mut self, off: usize, v: u32) {
        self.writes.push((off, v));
        match off {
            ULCON => self.ulcon = v,
            UCON => self.ucon = v,
            UFCON => self.ufcon = v,
            UTRSTAT => self.flags &= !(v & UTRSTAT_ALL_FLAGS),
            // A byte written into a full FIFO is lost, as on the hardware.
            UTXH if self.tx_fifo.len() < FIFO_SIZE as usize => self.tx_fifo.push_back(v as u8),
            _ => {}
        }
    }
}

impl SimUart {
    /// `write32` for a test that sets a register up without it counting as a driver write.
    pub fn write32_for_test(&mut self, off: usize, v: u32) {
        let n = self.writes.len();
        self.write32(off, v);
        self.writes.truncate(n);
    }

    pub fn read32_for_test(&self, off: usize) -> u32 {
        self.read32(off)
    }
}
