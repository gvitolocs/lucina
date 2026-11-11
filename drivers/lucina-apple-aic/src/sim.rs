// SPDX-License-Identifier: GPL-2.0-or-later
// Derived from drivers/irqchip/irq-apple-aic.c of AsahiLinux/linux (branch asahi, rev 6ba0637d1ed9):
// Copyright The Asahi Linux Contributors; based on irq-lpc32xx (Copyright 2015-2016 Vladimir Zapolskiy) and
// irq-bcm2836 (Copyright 2015 Broadcom). Behaviour re-expressed in Rust for LucinaOS; see the crate README.
// This is a Rust port of the behaviour of drivers/irqchip/irq-apple-aic.c; the original is the source of truth.

//! A software AIC and CPU for the tests. What it models is what the C states about the hardware:
//!
//! - one mask bit per IRQ, set through MASK_SET and cleared through MASK_CLR by writing ones (irq-apple-aic.c:15, :369-389);
//!   software triggers likewise through SW_SET / SW_CLR, ORed with the hardware line (:18);
//! - reading the event register acknowledges and masks what it returns (:17, :23, :394-395), lower IRQ numbers first (:21);
//! - on v1, the two per-CPU IPIs: `AIC_IPI_SEND` raises "other" on the CPUs named, `AIC_IPI_ACK` clears pending bits, the
//!   `AIC_IPI_MASK_*` registers set and clear mask bits (:19-20, :812-844, :902-913);
//! - `IPI_SR` is cleared by writing `IPI_SR_PENDING` to it (the acknowledge at :822 and :865).
//!
//! Assumptions of the model, not of the C (they only order the tests' events): all masks start clear; die 0 is drained
//! before die 1; a pending IRQ is returned before the v1 "other" IPI; reading MASK_SET / MASK_CLR returns the mask word.

use core::cell::RefCell;
use std::collections::BTreeMap;
use std::vec;
use std::vec::Vec;

use crate::hwirq::irq_hwirq;
use crate::info::{AicInfo, Caps, Geometry, Version, derive_geometry};
use crate::ipi_mux::{IpiMuxCpu, for_each_ipi};
use crate::mmio::{AicHandler, AicMmio, AicSysregs, Report, Sysreg};
use crate::regs::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Win {
    Core,
    Event,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MmioOp {
    Read(Win, usize, u32),
    Write(Win, usize, u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Array {
    SwSet,
    SwClr,
    MaskSet,
    MaskClr,
    HwState,
}

pub struct St {
    pub info: AicInfo,
    pub geo: Geometry,
    pub regs: BTreeMap<usize, u32>,
    /// `[die][word]` bit arrays.
    pub hw: Vec<Vec<u32>>,
    pub sw: Vec<Vec<u32>>,
    pub mask: Vec<Vec<u32>>,
    /// v1 IPI state of the CPU the core window is viewed from (`AIC_IPI_OTHER` / `AIC_IPI_SELF` bits).
    pub ipi_pending: u32,
    pub ipi_mask: u32,
    pub whoami: u32,
    pub log: Vec<MmioOp>,
}

pub struct SimAic {
    pub st: RefCell<St>,
}

#[derive(Clone, Copy)]
pub struct SimWindow<'a> {
    sim: &'a SimAic,
    win: Win,
}

impl SimAic {
    pub fn new(info: AicInfo, caps: Caps) -> SimAic {
        let geo = derive_geometry(&info, caps).expect("caps of the info's generation");
        let mut regs = BTreeMap::new();
        match caps {
            Caps::V1 { aic_info } => {
                regs.insert(AIC_INFO as usize, aic_info);
            }
            Caps::V2 { info1, info3 } => {
                regs.insert(AIC2_INFO1 as usize, info1);
                regs.insert(AIC2_INFO3 as usize, info3);
            }
        }
        let words = (geo.max_irq >> 5).max(bits_to_u32(geo.nr_irq)) as usize;
        let dies = geo.nr_die as usize;
        SimAic {
            st: RefCell::new(St {
                info,
                geo,
                regs,
                hw: vec![vec![0; words]; dies],
                sw: vec![vec![0; words]; dies],
                mask: vec![vec![0; words]; dies],
                ipi_pending: 0,
                ipi_mask: 0,
                whoami: 0,
                log: Vec::new(),
            }),
        }
    }

    pub fn core(&self) -> SimWindow<'_> {
        SimWindow {
            sim: self,
            win: Win::Core,
        }
    }

    /// The window `aic_handle_irq` reads the event from: the core window on v1, the second one on v2/v3.
    pub fn event(&self) -> SimWindow<'_> {
        let win = if self.st.borrow().geo.event_in_core {
            Win::Core
        } else {
            Win::Event
        };
        SimWindow { sim: self, win }
    }

    pub fn set_line(&self, die: u32, irq: u32, level: bool) {
        let mut st = self.st.borrow_mut();
        let w = &mut st.hw[die as usize][(irq >> 5) as usize];
        if level {
            *w |= 1 << (irq & 31);
        } else {
            *w &= !(1 << (irq & 31));
        }
    }

    pub fn masked(&self, die: u32, irq: u32) -> bool {
        self.st.borrow().mask[die as usize][(irq >> 5) as usize] & (1 << (irq & 31)) != 0
    }

    pub fn writes(&self) -> Vec<(usize, u32)> {
        self.st
            .borrow()
            .log
            .iter()
            .filter_map(|op| match *op {
                MmioOp::Write(_, off, v) => Some((off, v)),
                MmioOp::Read(..) => None,
            })
            .collect()
    }

    pub fn clear_log(&self) {
        self.st.borrow_mut().log.clear();
    }
}

impl St {
    fn locate(&self, off: usize) -> Option<(Array, usize, usize)> {
        let g = self.geo;
        let words = (g.max_irq >> 5) as usize;
        for die in 0..g.nr_die as usize {
            let d = die * g.die_stride as usize;
            for (base, a) in [
                (g.sw_set, Array::SwSet),
                (g.sw_clr, Array::SwClr),
                (g.mask_set, Array::MaskSet),
                (g.mask_clr, Array::MaskClr),
                (g.hw_state, Array::HwState),
            ] {
                let start = base as usize + d;
                if off >= start && off < start + 4 * words && (off - start).is_multiple_of(4) {
                    return Some((a, die, (off - start) / 4));
                }
            }
        }
        None
    }

    fn take_event(&mut self) -> u32 {
        let g = self.geo;
        for die in 0..g.nr_die {
            for n in 0..g.nr_irq {
                let (w, b) = ((n >> 5) as usize, 1u32 << (n & 31));
                let d = die as usize;
                if (self.hw[d][w] | self.sw[d][w]) & !self.mask[d][w] & b != 0 {
                    self.mask[d][w] |= b;
                    return irq_hwirq(die, n);
                }
            }
        }
        if self.info.version == Version::V1 {
            for (bit, num) in [
                (AIC_IPI_OTHER, AIC_EVENT_IPI_OTHER),
                (AIC_IPI_SELF, AIC_EVENT_IPI_SELF),
            ] {
                if self.ipi_pending & !self.ipi_mask & bit != 0 {
                    self.ipi_mask |= bit;
                    return (AIC_EVENT_TYPE_IPI << 16) | num;
                }
            }
        }
        0
    }

    fn read(&mut self, win: Win, off: usize) -> u32 {
        let v1 = self.info.version == Version::V1;
        let v = match win {
            Win::Event => {
                if off == self.info.event as usize {
                    self.take_event()
                } else {
                    0
                }
            }
            Win::Core if v1 && off == AIC_EVENT as usize => self.take_event(),
            Win::Core if v1 && off == AIC_WHOAMI as usize => self.whoami,
            Win::Core => match self.locate(off) {
                Some((Array::MaskSet | Array::MaskClr, d, w)) => self.mask[d][w],
                Some((Array::HwState, d, w)) => self.hw[d][w],
                Some((Array::SwSet | Array::SwClr, d, w)) => self.sw[d][w],
                None => self.regs.get(&off).copied().unwrap_or(0),
            },
        };
        self.log.push(MmioOp::Read(win, off, v));
        v
    }

    fn write(&mut self, win: Win, off: usize, v: u32) {
        self.log.push(MmioOp::Write(win, off, v));
        if win == Win::Event {
            return;
        }
        let v1 = self.info.version == Version::V1;
        if v1 && off == AIC_IPI_SEND as usize {
            if v & (1u32.checked_shl(self.whoami).unwrap_or(0)) != 0 {
                self.ipi_pending |= AIC_IPI_OTHER;
            }
            return;
        }
        if v1 && off == AIC_IPI_ACK as usize {
            self.ipi_pending &= !v;
            return;
        }
        if v1 && off == AIC_IPI_MASK_SET as usize {
            self.ipi_mask |= v;
            return;
        }
        if v1 && off == AIC_IPI_MASK_CLR as usize {
            self.ipi_mask &= !v;
            return;
        }
        match self.locate(off) {
            Some((Array::SwSet, d, w)) => self.sw[d][w] |= v,
            Some((Array::SwClr, d, w)) => self.sw[d][w] &= !v,
            Some((Array::MaskSet, d, w)) => self.mask[d][w] |= v,
            Some((Array::MaskClr, d, w)) => self.mask[d][w] &= !v,
            Some((Array::HwState, ..)) => {}
            None => {
                self.regs.insert(off, v);
            }
        }
    }
}

impl AicMmio for SimWindow<'_> {
    fn read32(&self, off: usize) -> u32 {
        self.sim.st.borrow_mut().read(self.win, off)
    }
    fn write32(&mut self, off: usize, v: u32) {
        self.sim.st.borrow_mut().write(self.win, off, v)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SysOp {
    Read(Sysreg),
    Write(Sysreg, u64),
    Isb,
}

#[derive(Default)]
pub struct SimSys {
    pub regs: BTreeMap<Sysreg, u64>,
    pub log: Vec<SysOp>,
}

impl SimSys {
    pub fn with(regs: &[(Sysreg, u64)]) -> SimSys {
        SimSys {
            regs: regs.iter().copied().collect(),
            log: Vec::new(),
        }
    }
    pub fn get(&self, r: Sysreg) -> u64 {
        self.regs.get(&r).copied().unwrap_or(0)
    }
    pub fn reads(&self) -> Vec<Sysreg> {
        self.log
            .iter()
            .filter_map(|op| {
                if let SysOp::Read(r) = op {
                    Some(*r)
                } else {
                    None
                }
            })
            .collect()
    }
}

impl AicSysregs for SimSys {
    fn read(&mut self, r: Sysreg) -> u64 {
        self.log.push(SysOp::Read(r));
        self.get(r)
    }
    fn write(&mut self, r: Sysreg, v: u64) {
        self.log.push(SysOp::Write(r, v));
        if r == Sysreg::IpiSr {
            // The acknowledge: writing IPI_SR_PENDING clears it (irq-apple-aic.c:822, :865).
            let old = self.get(r);
            self.regs.insert(r, old & !v);
        } else {
            self.regs.insert(r, v);
        }
    }
    fn isb(&mut self) {
        self.log.push(SysOp::Isb);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ev {
    Hwirq(u32),
    Mux,
    Report(Report),
}

/// The kernel's interrupt core as the tests see it: records what is handed to it, and runs this CPU's IPI mux.
#[derive(Default)]
pub struct Rec {
    pub events: Vec<Ev>,
    pub mux: IpiMuxCpu,
    pub delivered: Vec<u32>,
}

impl Rec {
    pub fn hwirqs(&self) -> Vec<u32> {
        self.events
            .iter()
            .filter_map(|e| if let Ev::Hwirq(h) = e { Some(*h) } else { None })
            .collect()
    }
}

impl AicHandler for Rec {
    fn handle_hwirq(&mut self, hwirq: u32) {
        self.events.push(Ev::Hwirq(hwirq));
    }
    fn ipi_mux_process(&mut self) {
        self.events.push(Ev::Mux);
        let ipis = self.mux.process();
        for_each_ipi(ipis, |n| self.delivered.push(n));
    }
    fn report(&mut self, r: Report) {
        self.events.push(Ev::Report(r));
    }
}
