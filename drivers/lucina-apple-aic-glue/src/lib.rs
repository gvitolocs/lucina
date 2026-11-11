// SPDX-License-Identifier: GPL-2.0-only
// Derived behaviour from drivers/irqchip/irq-apple-aic.c (GPL-2.0-or-later; Copyright The Asahi Linux Contributors) and
// drivers/clocksource/arm_arch_timer.c (GPL-2.0-only; Copyright (C) 2011 ARM Ltd.) of AsahiLinux/linux, branch asahi, rev
// 6ba0637d1ed9. Re-expressed in Rust for LucinaOS; see the crate README.
// This is a Rust port of the behaviour of drivers/irqchip/irq-apple-aic.c and drivers/clocksource/arm_arch_timer.c; the original is the source of truth.

//! The kernel side of the Apple Interrupt Controller, as pure logic: what LucinaOS's `ostd` AIC chip does around
//! [`lucina_apple_aic`] that is not Linux's irq-domain plumbing but still decides behaviour.
//!
//! - [`windows`]: which MMIO windows a device-tree node gives (core, and the event window of v2/v3);
//! - [`FiqAffinity`]: the `affinities` children (`build_fiq_affinity`, irq-apple-aic.c:928-964);
//! - [`LineTable`]: which kernel IRQ line each hwirq is mapped to;
//! - [`map`], [`unmap`], [`set_irq_enabled`], [`complete`], [`cpu_start`]: what `request_irq`/`enable_percpu_irq`, the
//!   flow handlers' ack and eoi, and the cpuhp starting callback do with the AIC ops;
//! - [`Dispatch`]: the [`AicHandler`] the IRQ and FIQ entry points hand hwirqs to;
//! - [`timer_interrupt_index`]: which of the arch timer node's interrupts the kernel's (virtual) timer uses.
//!
//! The kernel implements [`AicMmio`] and [`AicSysregs`] over the hardware; the tests run everything over the crate's
//! software AIC (`lucina_apple_aic::sim`).
#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::{boxed::Box, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use lucina_apple_aic::fwspec::{chip_for, decode, fiq_affinity_index, fiq_set_type, irq_set_type};
use lucina_apple_aic::hwirq::{fiq_hwirq, hwirq_type};
use lucina_apple_aic::regs::{AIC_EVENT_TYPE_FIQ, AIC_NR_FIQ};
use lucina_apple_aic::{
    Aic, AicHandler, AicInfo, AicMmio, AicSysregs, ChipKind, Decoded, FiqShadow, Geometry, Report,
    TranslateError, Version,
};

#[cfg(test)]
extern crate std;
#[cfg(test)]
mod tests;

/// The value of a [`LineTable`] slot that holds no line. The kernel's IRQ line numbers are 0 to 254 (`IRQ_NUM_MAX`).
pub const NO_LINE: u8 = u8::MAX;

/// The value of a FIQ slot's affinity index when the specifier gave none (a hardware IRQ never has one).
const NO_INDEX: u8 = u8::MAX;

// ---- Device tree ----

/// Where the event register's window is, relative to the core window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventWindow {
    /// v1: the event register is in the core window (`irqc->event = irqc->base`, irq-apple-aic.c:1007).
    InCore,
    /// v2/v3, `reg` 1 lies inside `reg` 0 (as on the M4: `0x3_81040000 + 0x4000` inside `0x3_81000000 + 0x1cc000`,
    /// t8132.dtsi:212-213): the event window is that part of the core mapping. `of_iomap(node, 1)` (:1025) maps it a second
    /// time; one mapping sliced is the same memory.
    Slice { offset: u64, len: u64 },
    /// v2/v3, `reg` 1 elsewhere: a mapping of its own.
    Separate { base: u64, len: u64 },
}

/// Why a node's `reg` gives no usable windows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowError {
    /// No `reg` 0, or an empty one: `of_iomap(node, 0)` fails (irq-apple-aic.c:975-977).
    NoCore,
    /// v2/v3 without `reg` 1, or an empty one: `of_iomap(node, 1)` fails (irq-apple-aic.c:1025-1027).
    NoEvent,
}

/// The core window (`reg` 0) and the event window for an AIC of `info`'s generation, from the node's `reg` entries as
/// (address, size).
pub fn windows(
    info: &AicInfo,
    regs: &[(u64, u64)],
) -> Result<((u64, u64), EventWindow), WindowError> {
    let core = match regs.first() {
        Some(&(base, len)) if len != 0 => (base, len),
        _ => return Err(WindowError::NoCore),
    };
    if info.version == Version::V1 {
        return Ok((core, EventWindow::InCore));
    }
    let (base, len) = match regs.get(1) {
        Some(&(base, len)) if len != 0 => (base, len),
        _ => return Err(WindowError::NoEvent),
    };
    let inside = base >= core.0
        && base
            .checked_add(len)
            .zip(core.0.checked_add(core.1))
            .is_some_and(|(end, core_end)| end <= core_end);
    let event = if inside {
        EventWindow::Slice {
            offset: base - core.0,
            len,
        }
    } else {
        EventWindow::Separate { base, len }
    };
    Ok((core, event))
}

/// The per-FIQ CPU sets of the node's `affinities` children (`ic->fiq_aff`, irq-apple-aic.c:344, built at :1057-1063), as
/// MPIDR affinity values. The C keeps logical CPU numbers, found from the `cpus` nodes with `of_cpu_node_to_id` (:952),
/// which maps a CPU node to the CPU whose MPIDR is its `reg`; the MPIDR is kept here instead, so that the set can be built
/// before the other CPUs have numbers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FiqAffinity {
    sets: [Option<Vec<u64>>; AIC_NR_FIQ as usize],
}

impl FiqAffinity {
    pub fn new() -> Self {
        Self::default()
    }

    /// `build_fiq_affinity` for one child (irq-apple-aic.c:928-964). `fiq_index` is its `apple,fiq-index` and `cpus` its
    /// `cpus` phandles (`None` when the property is missing); `resolve` gives the MPIDR affinity of a phandle's CPU, or
    /// `None` when there is no such CPU node.
    ///
    /// As the C: a child without an index, with an index of `AIC_NR_FIQ` or more, or for an index already set is ignored
    /// (:933-935); without `cpus` it is ignored (:937-939); a phandle that names no CPU is skipped (:950-956). A child with
    /// an empty `cpus` sets an empty set: that FIQ is enabled on no CPU.
    pub fn add_child(
        &mut self,
        fiq_index: Option<u32>,
        cpus: Option<&[u32]>,
        resolve: impl Fn(u32) -> Option<u64>,
    ) {
        let Some(fiq) = fiq_index else {
            return;
        };
        if fiq >= AIC_NR_FIQ || self.sets[fiq as usize].is_some() {
            return;
        }
        let Some(cpus) = cpus else {
            return;
        };
        let set = cpus
            .iter()
            .filter_map(|&phandle| resolve(phandle))
            .collect();
        self.sets[fiq as usize] = Some(set);
    }

    /// The CPUs of FIQ binding index `fiq_index`, if its affinity was given.
    pub fn get(&self, fiq_index: u32) -> Option<&[u64]> {
        self.sets.get(fiq_index as usize)?.as_deref()
    }

    /// Whether a per-CPU interrupt whose specifier had affinity index `fiq_index` is enabled on the CPU with MPIDR
    /// affinity `mpidr`: every CPU when the index has no set (hardware IRQs and FIQs without an `affinities` child; the C
    /// gives such an interrupt no partition, :672-677), else the CPUs of its set (`irq_set_percpu_devid_partition`,
    /// :647-648).
    pub fn allows(&self, fiq_index: Option<u32>, mpidr: u64) -> bool {
        match fiq_index.and_then(|i| self.get(i)) {
            None => true,
            Some(set) => set.contains(&mpidr),
        }
    }
}

/// Which of the arch timer node's `interrupts` the kernel's timer uses, given its `interrupt-names` property (the raw,
/// NUL-separated strings; `None` when absent) and the number of specifiers.
///
/// LucinaOS's timer is the EL1 virtual timer (`cntv_*_el0`). Linux finds that interrupt by the name `"virt"` when the node
/// has `interrupt-names` (arm_arch_timer.c:1174-1180 with the names at :46-51), and otherwise by its position,
/// `ARCH_TIMER_VIRT_PPI` = 2 (include/clocksource/arm_arch_timer.h:34-41). The M4's node names its four FIQs `"phys",
/// "virt", "hyp-phys", "hyp-virt"` (t8132.dtsi:458-462): by position the third would be the hypervisor timer. `None` when
/// the interrupt is not there, as `of_irq_get_byname` failing (Linux then has no interrupt for that timer).
pub fn timer_interrupt_index(names: Option<&[u8]>, count: usize) -> Option<usize> {
    const VIRT: &[u8] = b"virt";
    const VIRT_POSITION: usize = 2;
    timer_interrupt_by(names, count, VIRT, VIRT_POSITION)
}

/// [`timer_interrupt_index`] for a kernel that runs at EL2 as a VHE host (LucinaOS patch 2040), where `cntv_*_el0` is the
/// EL2 virtual timer (CNTHV_*_EL2): the interrupt named `"hyp-virt"` when the node has `interrupt-names`, else the fifth,
/// `ARCH_TIMER_HYP_VIRT_PPI` = 4 (include/clocksource/arm_arch_timer.h:34-41; the names at arm_arch_timer.c:46-51), as
/// Linux's `arch_timer_select_ppi` picks for a kernel in hyp mode (arm_arch_timer.c:1136-1145). The M4's node has it
/// fourth, by name (t8132.dtsi:458-462); QEMU virt with `virtualization=on` gives five without names. `None` when it is
/// not there: Linux then falls back to the EL2 physical timer (`ARCH_TIMER_HYP_PPI`, :1143-1144), which a kernel that
/// uses the virtual timer's registers cannot.
pub fn hyp_timer_interrupt_index(names: Option<&[u8]>, count: usize) -> Option<usize> {
    const HYP_VIRT: &[u8] = b"hyp-virt";
    const HYP_VIRT_POSITION: usize = 4;
    timer_interrupt_by(names, count, HYP_VIRT, HYP_VIRT_POSITION)
}

/// The index of the interrupt called `name` in `names`, else `position` when there are no names; `None` unless it is
/// below `count`.
fn timer_interrupt_by(
    names: Option<&[u8]>,
    count: usize,
    name: &[u8],
    position: usize,
) -> Option<usize> {
    let index = match names {
        Some(names) => names
            .split(|&b| b == 0)
            .take_while(|s| !s.is_empty())
            .position(|n| n == name)?,
        None => position,
    };
    (index < count).then_some(index)
}

// ---- Lines ----

/// Why a specifier could not be mapped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapError {
    /// `aic_irq_domain_translate` refused it.
    Translate(TranslateError),
    /// The trigger type is not one the chip takes (`aic_irq_set_type` / `aic_fiq_set_type`).
    Type,
    /// Already mapped to a line.
    Taken,
    /// A hwirq the table has no slot for (outside this AIC's counts).
    NoSlot,
}

/// Which kernel IRQ line each hwirq is mapped to (the C's irq domain, a tree domain, irq-apple-aic.c:1051): one slot per
/// FIQ index below `AIC_NR_FIQ` and one per IRQ below `nr_irq` of each die below `nr_die`, the hwirqs `translate` accepts.
pub struct LineTable {
    fiq: [AtomicU8; AIC_NR_FIQ as usize],
    /// The affinity index of each FIQ slot's specifier ([`fiq_affinity_index`]), [`NO_INDEX`] for none.
    fiq_index: [AtomicU8; AIC_NR_FIQ as usize],
    irq: Box<[AtomicU8]>,
    /// The core's mask of each IRQ (`irqd_irq_masked`): a masked IRQ is not unmasked by its eoi (irq-apple-aic.c:391-399).
    irq_masked: Box<[AtomicBool]>,
    nr_irq: u32,
    nr_die: u32,
}

impl LineTable {
    pub fn new(geo: &Geometry) -> Self {
        let n = (geo.nr_die as usize).saturating_mul(geo.nr_irq as usize);
        Self {
            fiq: [const { AtomicU8::new(NO_LINE) }; AIC_NR_FIQ as usize],
            fiq_index: [const { AtomicU8::new(NO_INDEX) }; AIC_NR_FIQ as usize],
            irq: (0..n).map(|_| AtomicU8::new(NO_LINE)).collect(),
            irq_masked: (0..n).map(|_| AtomicBool::new(false)).collect(),
            nr_irq: geo.nr_irq,
            nr_die: geo.nr_die,
        }
    }

    fn irq_slot(&self, hwirq: u32) -> Option<usize> {
        match decode(hwirq) {
            Decoded::Irq { die, num } if die < self.nr_die && num < self.nr_irq => {
                Some(die as usize * self.nr_irq as usize + num as usize)
            }
            _ => None,
        }
    }

    fn fiq_slot(hwirq: u32) -> Option<usize> {
        match decode(hwirq) {
            Decoded::Fiq { num } if num < AIC_NR_FIQ => Some(num as usize),
            _ => None,
        }
    }

    fn slot(&self, hwirq: u32) -> Option<&AtomicU8> {
        if let Some(i) = Self::fiq_slot(hwirq) {
            return Some(&self.fiq[i]);
        }
        self.irq_slot(hwirq).map(|i| &self.irq[i])
    }

    /// The line `hwirq` is mapped to.
    pub fn get(&self, hwirq: u32) -> Option<u8> {
        let line = self.slot(hwirq)?.load(Ordering::Acquire);
        (line != NO_LINE).then_some(line)
    }

    /// Records `hwirq` as mapped to `line`, with the affinity index of its specifier (FIQs only).
    pub fn claim(&self, hwirq: u32, line: u8, fiq_index: Option<u32>) -> Result<(), MapError> {
        if line == NO_LINE {
            return Err(MapError::NoSlot);
        }
        let slot = self.slot(hwirq).ok_or(MapError::NoSlot)?;
        if let Some(i) = Self::fiq_slot(hwirq) {
            let index = fiq_index
                .filter(|&i| i < u32::from(NO_INDEX))
                .map_or(NO_INDEX, |i| i as u8);
            // Written before the line, which publishes it.
            self.fiq_index[i].store(index, Ordering::Relaxed);
        }
        if let Some(i) = self.irq_slot(hwirq) {
            self.irq_masked[i].store(false, Ordering::Relaxed);
        }
        slot.compare_exchange(NO_LINE, line, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| ())
            .map_err(|_| MapError::Taken)
    }

    /// Forgets `hwirq`'s line.
    pub fn release(&self, hwirq: u32) {
        if let Some(slot) = self.slot(hwirq) {
            slot.store(NO_LINE, Ordering::Release);
        }
    }

    /// Whether the core has masked IRQ `hwirq` ([`set_irq_enabled`]); an unmapped IRQ counts as masked (Linux leaves an
    /// event nobody handles masked: `generic_handle_domain_irq` fails and no eoi follows, irq-apple-aic.c:416).
    pub fn irq_masked(&self, hwirq: u32) -> bool {
        match self.irq_slot(hwirq) {
            Some(i) => {
                self.irq[i].load(Ordering::Acquire) == NO_LINE
                    || self.irq_masked[i].load(Ordering::Acquire)
            }
            None => true,
        }
    }

    fn set_irq_masked(&self, hwirq: u32, masked: bool) {
        if let Some(i) = self.irq_slot(hwirq) {
            self.irq_masked[i].store(masked, Ordering::Release);
        }
    }

    /// The mapped FIQs, as (hwirq, affinity index of the specifier).
    pub fn mapped_fiqs(&self) -> impl Iterator<Item = (u32, Option<u32>)> + '_ {
        (0..AIC_NR_FIQ).filter_map(move |num| {
            let i = num as usize;
            if self.fiq[i].load(Ordering::Acquire) == NO_LINE {
                return None;
            }
            let index = self.fiq_index[i].load(Ordering::Relaxed);
            Some((
                fiq_hwirq(num),
                (index != NO_INDEX).then_some(u32::from(index)),
            ))
        })
    }
}

/// A mapped specifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mapped {
    pub hwirq: u32,
    pub kind: ChipKind,
}

/// Maps the specifier `cells` to `line` and enables it, as `irq_create_of_mapping` then `request_irq` (hardware IRQs:
/// unmasked at the AIC, `aic_irq_unmask`) or `request_percpu_irq` + `enable_percpu_irq` on this CPU (FIQs: `aic_fiq_unmask`
/// on the CPU with MPIDR affinity `mpidr`, if the specifier's affinity includes it). The other CPUs enable a mapped FIQ in
/// [`cpu_start`]: an FIQ's masks are this CPU's system registers.
///
/// The trigger is checked as the chip's `irq_set_type` does: level-high or rising edge for an IRQ, level-high for a FIQ.
#[allow(clippy::too_many_arguments)]
pub fn map<C: AicMmio, S: AicSysregs>(
    aic: &Aic,
    lines: &LineTable,
    aff: &FiqAffinity,
    core: &mut C,
    sys: &mut S,
    shadow: &mut FiqShadow,
    mpidr: u64,
    cells: &[u32],
    line: u8,
) -> Result<Mapped, MapError> {
    let t = aic.translate(cells).map_err(MapError::Translate)?;
    let kind = chip_for(aic.info.version, t.hwirq);
    match kind {
        ChipKind::Fiq => fiq_set_type(t.sense),
        ChipKind::Aic | ChipKind::Aic2 => irq_set_type(t.sense),
    }
    .map_err(|_| MapError::Type)?;
    let index = fiq_affinity_index(cells);
    lines.claim(t.hwirq, line, index)?;
    match kind {
        ChipKind::Fiq => {
            if aff.allows(index, mpidr) {
                aic.fiq_unmask(sys, shadow, t.hwirq);
            }
        }
        ChipKind::Aic | ChipKind::Aic2 => aic.irq_unmask(core, t.hwirq),
    }
    Ok(Mapped {
        hwirq: t.hwirq,
        kind,
    })
}

/// Unmaps `hwirq`: masked (an IRQ at the AIC; a FIQ on this CPU), then its line forgotten.
pub fn unmap<C: AicMmio, S: AicSysregs>(
    aic: &Aic,
    lines: &LineTable,
    core: &mut C,
    sys: &mut S,
    shadow: &mut FiqShadow,
    hwirq: u32,
) {
    if hwirq_type(hwirq) == AIC_EVENT_TYPE_FIQ {
        aic.fiq_mask(sys, shadow, hwirq);
    } else {
        aic.irq_mask(core, hwirq);
    }
    lines.release(hwirq);
}

/// `disable_irq` / `enable_irq` of a mapped hardware IRQ: masked or unmasked at the AIC, and remembered, so that the eoi
/// leaves a masked IRQ masked. FIQs are not taken (their masks are per CPU).
pub fn set_irq_enabled<C: AicMmio>(
    aic: &Aic,
    lines: &LineTable,
    core: &mut C,
    hwirq: u32,
    enabled: bool,
) {
    if hwirq_type(hwirq) == AIC_EVENT_TYPE_FIQ || lines.get(hwirq).is_none() {
        return;
    }
    lines.set_irq_masked(hwirq, !enabled);
    if enabled {
        aic.irq_unmask(core, hwirq);
    } else {
        aic.irq_mask(core, hwirq);
    }
}

/// The end of a handled interrupt (the flow handlers' `irq_eoi`): `aic_irq_eoi` for an IRQ, which unmasks what the event
/// read masked unless the core has it masked (irq-apple-aic.c:391-399); `aic_fiq_eoi` for a FIQ, which lifts the mask the
/// ack put on if the core has it unmasked on this CPU (:545-550).
pub fn complete<C: AicMmio, S: AicSysregs>(
    aic: &Aic,
    lines: &LineTable,
    core: &mut C,
    sys: &mut S,
    shadow: &FiqShadow,
    hwirq: u32,
) {
    match decode(hwirq) {
        Decoded::Irq { .. } => aic.irq_eoi(core, hwirq, lines.irq_masked(hwirq)),
        Decoded::Fiq { .. } => aic.fiq_eoi(sys, shadow, hwirq),
        Decoded::Other { .. } => {}
    }
}

/// A CPU's start (`aic_init_cpu`, run by the cpuhp starting state on every CPU, the boot CPU included,
/// irq-apple-aic.c:1094-1096), followed by what that CPU's `enable_percpu_irq` calls do for the FIQs mapped so far: each
/// whose affinity includes the CPU (MPIDR affinity `mpidr`) is unmasked on it. Returns the CPU's FIQ shadow.
#[allow(clippy::too_many_arguments)]
pub fn cpu_start<C: AicMmio, S: AicSysregs, H: AicHandler>(
    aic: &Aic,
    lines: &LineTable,
    aff: &FiqAffinity,
    core: &mut C,
    sys: &mut S,
    h: &mut H,
    cpu: u32,
    mpidr: u64,
) -> FiqShadow {
    let mut shadow = aic.init_cpu(core, sys, h, cpu);
    for (hwirq, index) in lines.mapped_fiqs() {
        if aff.allows(index, mpidr) {
            aic.fiq_unmask(sys, &mut shadow, hwirq);
        }
    }
    shadow
}

// ---- Dispatch ----

/// What the IRQ and FIQ entry points ([`Aic::handle_irq`], [`Aic::handle_fiq`]) hand their hwirqs to.
///
/// A mapped hwirq is delivered to `deliver(hwirq, line)`: for a FIQ after its ack, `aic_fiq_set_mask` (the FIQ chip's
/// `irq_ack`, irq-apple-aic.c:624, called first by `handle_percpu_devid_irq`); for an IRQ directly (`handle_fasteoi_irq`
/// has no ack). The kernel's delivery runs the line's callbacks and then its eoi ([`complete`]). An unmapped hwirq is
/// counted and left as it is, as in Linux, where `generic_handle_domain_irq` fails (an IRQ stays masked by its event read).
///
/// The virtual IPIs are not used yet: the mux is counted and nothing is delivered. Reports are counted and the last kept.
pub struct Dispatch<'a, S: AicSysregs, F: FnMut(u32, u8)> {
    pub aic: &'a Aic,
    pub lines: &'a LineTable,
    /// The CPU's system registers, for the FIQ ack.
    pub sys: S,
    pub deliver: F,
    pub unmapped: u32,
    pub ipis: u32,
    pub reports: u32,
    pub last_report: Option<Report>,
}

impl<'a, S: AicSysregs, F: FnMut(u32, u8)> Dispatch<'a, S, F> {
    pub fn new(aic: &'a Aic, lines: &'a LineTable, sys: S, deliver: F) -> Self {
        Self {
            aic,
            lines,
            sys,
            deliver,
            unmapped: 0,
            ipis: 0,
            reports: 0,
            last_report: None,
        }
    }
}

impl<S: AicSysregs, F: FnMut(u32, u8)> AicHandler for Dispatch<'_, S, F> {
    fn handle_hwirq(&mut self, hwirq: u32) {
        let Some(line) = self.lines.get(hwirq) else {
            self.unmapped = self.unmapped.saturating_add(1);
            return;
        };
        if hwirq_type(hwirq) == AIC_EVENT_TYPE_FIQ {
            self.aic.fiq_set_mask(&mut self.sys, hwirq);
        }
        (self.deliver)(hwirq, line);
    }

    fn ipi_mux_process(&mut self) {
        self.ipis = self.ipis.saturating_add(1);
    }

    fn report(&mut self, r: Report) {
        self.reports = self.reports.saturating_add(1);
        self.last_report = Some(r);
    }
}
