// SPDX-License-Identifier: GPL-2.0-only
// Derived behaviour from drivers/irqchip/irq-apple-aic.c (GPL-2.0-or-later; Copyright The Asahi Linux Contributors) and
// drivers/clocksource/arm_arch_timer.c (GPL-2.0-only; Copyright (C) 2011 ARM Ltd.) of AsahiLinux/linux, branch asahi, rev
// 6ba0637d1ed9. Re-expressed in Rust for LucinaOS; see the crate README.
// This is a Rust port of the behaviour of drivers/irqchip/irq-apple-aic.c and drivers/clocksource/arm_arch_timer.c; the original is the source of truth.
// Tests of the glue over lucina-apple-aic's software AIC (`sim`). The geometry numbers are synthetic, as in that crate's
// tests: they claim nothing about a Mac.

use core::cell::RefCell;
use std::vec;
use std::vec::Vec;

use lucina_apple_aic::hwirq::{fiq_hwirq, hwirq_irq, irq_hwirq};
use lucina_apple_aic::info::{AIC1_INFO, AIC3_INFO, derive_geometry};
use lucina_apple_aic::regs::*;
use lucina_apple_aic::sim::{SimAic, SimSys, SysOp};
use lucina_apple_aic::{
    Aic, AicSysregs, Caps, ChipKind, FiqShadow, Report, Sysreg, TranslateError,
};

use super::*;

/// Synthetic single-die v3 caps: 512 IRQs of 1024 (as lucina-apple-aic's V3_CAPS).
const V3_CAPS: Caps = Caps::V2 {
    info1: 512,
    info3: 1024,
};
/// Synthetic two-die caps: 600 IRQs of 640 per die.
const V3_TWO_DIES: Caps = Caps::V2 {
    info1: (1 << 24) | 600,
    info3: (2 << 24) | 640,
};

/// The MPIDR affinity values of two CPUs of the tests.
const CPU_A: u64 = 0x0000;
const CPU_B: u64 = 0x0101;

fn aic(caps: Caps, el2: bool) -> Aic {
    Aic {
        info: AIC3_INFO,
        geo: derive_geometry(&AIC3_INFO, caps).unwrap(),
        el2,
    }
}

/// One CPU's system registers shared between the entry point and the dispatch's ack, as the hardware registers are.
struct Shared<'a>(&'a RefCell<SimSys>);

impl AicSysregs for Shared<'_> {
    fn read(&mut self, r: Sysreg) -> u64 {
        self.0.borrow_mut().read(r)
    }
    fn write(&mut self, r: Sysreg, v: u64) {
        self.0.borrow_mut().write(r, v)
    }
    fn isb(&mut self) {
        self.0.borrow_mut().isb()
    }
}

/// A handler for `init_cpu`'s reports.
#[derive(Default)]
struct Reports(Vec<Report>);

impl AicHandler for Reports {
    fn handle_hwirq(&mut self, _hwirq: u32) {}
    fn ipi_mux_process(&mut self) {}
    fn report(&mut self, r: Report) {
        self.0.push(r);
    }
}

const ENABLED_FIRING: u64 = ARCH_TIMER_CTRL_ENABLE | ARCH_TIMER_CTRL_IT_STAT;

// ---- windows ----

#[test]
fn m4_event_window_is_a_slice_of_the_core_window() {
    // t8132.dtsi:212-213.
    let regs = [(0x3_8100_0000, 0x1c_c000), (0x3_8104_0000, 0x4000)];
    assert_eq!(
        windows(&AIC3_INFO, &regs),
        Ok((
            (0x3_8100_0000, 0x1c_c000),
            EventWindow::Slice {
                offset: 0x4_0000,
                len: 0x4000
            }
        ))
    );
}

#[test]
fn event_window_outside_the_core_is_separate() {
    let regs = [(0x1000_0000, 0x1_0000), (0x2000_0000, 0x4000)];
    assert_eq!(
        windows(&AIC3_INFO, &regs).unwrap().1,
        EventWindow::Separate {
            base: 0x2000_0000,
            len: 0x4000
        }
    );
    // Straddling the core's end: not inside.
    let regs = [(0x1000_0000, 0x1_0000), (0x1000_e000, 0x4000)];
    assert_eq!(
        windows(&AIC3_INFO, &regs).unwrap().1,
        EventWindow::Separate {
            base: 0x1000_e000,
            len: 0x4000
        }
    );
}

#[test]
fn windows_need_reg_0_and_on_v3_reg_1() {
    assert_eq!(windows(&AIC3_INFO, &[]), Err(WindowError::NoCore));
    assert_eq!(
        windows(&AIC3_INFO, &[(0x1000, 0)]),
        Err(WindowError::NoCore)
    );
    assert_eq!(
        windows(&AIC3_INFO, &[(0x1000, 0x1000)]),
        Err(WindowError::NoEvent)
    );
    assert_eq!(
        windows(&AIC3_INFO, &[(0x1000, 0x1000), (0x2000, 0)]),
        Err(WindowError::NoEvent)
    );
    // v1 has its event register in the core window and ignores a second entry.
    assert_eq!(
        windows(&AIC1_INFO, &[(0x1000, 0x8000)]),
        Ok(((0x1000, 0x8000), EventWindow::InCore))
    );
}

// ---- affinities ----

fn resolve(phandle: u32) -> Option<u64> {
    match phandle {
        10 => Some(CPU_A),
        11 => Some(CPU_B),
        _ => None,
    }
}

#[test]
fn affinities_follow_build_fiq_affinity() {
    let mut aff = FiqAffinity::new();
    // No index, an index past AIC_NR_FIQ, no cpus: ignored.
    aff.add_child(None, Some(&[10]), resolve);
    aff.add_child(Some(AIC_NR_FIQ), Some(&[10]), resolve);
    aff.add_child(Some(AIC_CPU_PMU_E), None, resolve);
    assert_eq!(aff, FiqAffinity::new());
    // A phandle that names no CPU is skipped.
    aff.add_child(Some(AIC_CPU_PMU_E), Some(&[10, 99]), resolve);
    assert_eq!(aff.get(AIC_CPU_PMU_E), Some(&[CPU_A][..]));
    // The first child for an index wins.
    aff.add_child(Some(AIC_CPU_PMU_E), Some(&[11]), resolve);
    assert_eq!(aff.get(AIC_CPU_PMU_E), Some(&[CPU_A][..]));
    // An empty cpus list is an empty set.
    aff.add_child(Some(AIC_CPU_PMU_P), Some(&[]), resolve);
    assert_eq!(aff.get(AIC_CPU_PMU_P), Some(&[][..]));

    assert!(aff.allows(Some(AIC_CPU_PMU_E), CPU_A));
    assert!(!aff.allows(Some(AIC_CPU_PMU_E), CPU_B));
    assert!(!aff.allows(Some(AIC_CPU_PMU_P), CPU_A));
    // No set for the index, or no index: every CPU.
    assert!(aff.allows(Some(AIC_TMR_GUEST_VIRT), CPU_B));
    assert!(aff.allows(None, CPU_B));
}

// ---- the timer's interrupt ----

#[test]
fn timer_interrupt_by_name_or_position() {
    // t8132.dtsi:458: four names, "virt" second.
    let m4 = b"phys\0virt\0hyp-phys\0hyp-virt\0";
    assert_eq!(timer_interrupt_index(Some(m4), 4), Some(1));
    // The binding order without names (QEMU virt, the Galaxy Book Go): the third.
    assert_eq!(timer_interrupt_index(None, 4), Some(2));
    assert_eq!(timer_interrupt_index(None, 3), Some(2));
    assert_eq!(timer_interrupt_index(None, 2), None);
    // Names without "virt", or naming more interrupts than there are.
    assert_eq!(timer_interrupt_index(Some(b"phys\0hyp-phys\0"), 2), None);
    assert_eq!(timer_interrupt_index(Some(b"phys\0virt\0"), 1), None);
    // An exact match only.
    assert_eq!(timer_interrupt_index(Some(b"virtual\0virt\0"), 2), Some(1));
}

#[test]
fn hyp_timer_interrupt_by_name_or_position() {
    // At EL2 (patch 2040). t8132.dtsi:458: "hyp-virt" fourth, the hypervisor's own virtual timer, AIC_TMR_HV_VIRT.
    let m4 = b"phys\0virt\0hyp-phys\0hyp-virt\0";
    assert_eq!(hyp_timer_interrupt_index(Some(m4), 4), Some(3));
    // Without names (QEMU virt with virtualization=on has five): the fifth, ARCH_TIMER_HYP_VIRT_PPI.
    assert_eq!(hyp_timer_interrupt_index(None, 5), Some(4));
    // Four without names (no EL2 virtual timer: Linux would take the EL2 physical one): none.
    assert_eq!(hyp_timer_interrupt_index(None, 4), None);
    // Names without "hyp-virt", or naming more interrupts than there are.
    assert_eq!(
        hyp_timer_interrupt_index(Some(b"phys\0virt\0hyp-phys\0"), 3),
        None
    );
    assert_eq!(hyp_timer_interrupt_index(Some(m4), 3), None);
    // An exact match only: "virt" is not "hyp-virt", nor the other way round.
    assert_eq!(
        hyp_timer_interrupt_index(Some(b"virt\0hyp-virtual\0hyp-virt\0"), 3),
        Some(2)
    );
    assert_eq!(timer_interrupt_index(Some(b"hyp-virt\0virt\0"), 2), Some(1));
}

// ---- lines ----

#[test]
fn line_table_slots_follow_the_counts() {
    let a = aic(V3_TWO_DIES, false);
    let lines = LineTable::new(&a.geo);
    assert_eq!(lines.claim(irq_hwirq(1, 599), 3, None), Ok(()));
    assert_eq!(lines.get(irq_hwirq(1, 599)), Some(3));
    assert_eq!(lines.get(irq_hwirq(0, 599)), None);
    assert_eq!(lines.claim(irq_hwirq(2, 0), 3, None), Err(MapError::NoSlot));
    assert_eq!(
        lines.claim(irq_hwirq(0, 600), 3, None),
        Err(MapError::NoSlot)
    );
    assert_eq!(
        lines.claim(fiq_hwirq(AIC_NR_FIQ), 3, None),
        Err(MapError::NoSlot)
    );
    assert_eq!(
        lines.claim(fiq_hwirq(1), NO_LINE, None),
        Err(MapError::NoSlot)
    );
    assert_eq!(
        lines.claim(irq_hwirq(1, 599), 4, None),
        Err(MapError::Taken)
    );
    lines.release(irq_hwirq(1, 599));
    assert_eq!(lines.get(irq_hwirq(1, 599)), None);
    assert_eq!(lines.claim(irq_hwirq(1, 599), 4, None), Ok(()));
}

#[test]
fn map_checks_translate_and_trigger() {
    let sim = SimAic::new(AIC3_INFO, V3_CAPS);
    let a = aic(V3_CAPS, false);
    let lines = LineTable::new(&a.geo);
    let aff = FiqAffinity::new();
    let mut sys = SimSys::default();
    let mut shadow = FiqShadow(0);
    let mut map_cells = |cells: &[u32], line| {
        map(
            &a,
            &lines,
            &aff,
            &mut sim.core(),
            &mut sys,
            &mut shadow,
            CPU_A,
            cells,
            line,
        )
    };
    // The hypervisor timers do not exist at EL1 (irq-apple-aic.c:730-732).
    assert_eq!(
        map_cells(&[DT_AIC_FIQ, AIC_TMR_HV_VIRT, IRQ_TYPE_LEVEL_HIGH], 1),
        Err(MapError::Translate(TranslateError::NoEntry))
    );
    // A FIQ is level-high only; an IRQ level-high or rising edge.
    assert_eq!(
        map_cells(&[DT_AIC_FIQ, AIC_TMR_GUEST_VIRT, IRQ_TYPE_EDGE_RISING], 1),
        Err(MapError::Type)
    );
    assert_eq!(map_cells(&[DT_AIC_IRQ, 5, 8], 1), Err(MapError::Type));
    assert_eq!(
        map_cells(&[DT_AIC_IRQ, 5, IRQ_TYPE_EDGE_RISING], 1),
        Ok(Mapped {
            hwirq: irq_hwirq(0, 5),
            kind: ChipKind::Aic2
        })
    );
    assert_eq!(
        map_cells(&[DT_AIC_IRQ, 5, IRQ_TYPE_LEVEL_HIGH], 2),
        Err(MapError::Taken)
    );
    // Nothing was claimed by the refused ones.
    assert_eq!(lines.mapped_fiqs().count(), 0);
}

// ---- the timer FIQ end to end ----

/// The M4's virtual timer specifier (t8132.dtsi:459) mapped at EL1, the timer firing, the FIQ entry point, the delivery and
/// the eoi.
#[test]
fn el1_virtual_timer_fiq_is_delivered_once_per_entry() {
    let sim = SimAic::new(AIC3_INFO, V3_CAPS);
    let a = aic(V3_CAPS, false);
    let lines = LineTable::new(&a.geo);
    let aff = FiqAffinity::new();
    let sys = RefCell::new(SimSys::default());

    let mut shadow = cpu_start(
        &a,
        &lines,
        &aff,
        &mut sim.core(),
        &mut Shared(&sys),
        &mut Reports::default(),
        0,
        CPU_A,
    );
    // aic_init_cpu masked both EL0 timers (irq-apple-aic.c:868-869).
    assert_eq!(
        sys.borrow().get(Sysreg::CntvCtlEl0) & ARCH_TIMER_CTRL_IT_MASK,
        ARCH_TIMER_CTRL_IT_MASK
    );

    let m = map(
        &a,
        &lines,
        &aff,
        &mut sim.core(),
        &mut Shared(&sys),
        &mut shadow,
        CPU_A,
        &[DT_AIC_FIQ, AIC_TMR_GUEST_VIRT, IRQ_TYPE_LEVEL_HIGH],
        7,
    )
    .unwrap();
    // At EL1 the guest virtual timer is the EL0 virtual timer (irq-apple-aic.c:724-729).
    assert_eq!(
        m,
        Mapped {
            hwirq: fiq_hwirq(FIQ_TMR_EL0_VIRT),
            kind: ChipKind::Fiq
        }
    );
    assert_eq!(shadow, FiqShadow(1 << FIQ_TMR_EL0_VIRT));

    // The kernel enables its timer (ENABLE, IMASK clear) and it fires.
    sys.borrow_mut()
        .regs
        .insert(Sysreg::CntvCtlEl0, ENABLED_FIRING);
    sys.borrow_mut().log.clear();
    let mut delivered = Vec::new();
    let mut d = Dispatch::new(&a, &lines, Shared(&sys), |hwirq, line| {
        delivered.push((hwirq, line))
    });
    a.handle_fiq(&mut sim.core(), &mut Shared(&sys), &mut d);
    assert_eq!((d.unmapped, d.ipis, d.reports), (0, 0, 0));
    assert_eq!(delivered, vec![(fiq_hwirq(FIQ_TMR_EL0_VIRT), 7)]);
    // At EL1 nothing was written: the EL0 timers have no AIC mask (aic_fiq_set_mask, irq-apple-aic.c:500-515), and the
    // guest-timer enables (an EL2 register) were not read.
    let ops = sys.borrow().log.clone();
    assert!(ops.iter().all(|op| !matches!(op, SysOp::Write(..))));
    assert!(!ops.contains(&SysOp::Read(Sysreg::VmTmrFiqEna)));

    // The eoi writes nothing for an EL0 timer either; the timer itself stops when the kernel sets its next compare value.
    sys.borrow_mut().log.clear();
    complete(
        &a,
        &lines,
        &mut sim.core(),
        &mut Shared(&sys),
        &shadow,
        fiq_hwirq(FIQ_TMR_EL0_VIRT),
    );
    assert!(
        sys.borrow()
            .log
            .iter()
            .all(|op| !matches!(op, SysOp::Write(..)))
    );
    assert!(sim.writes().is_empty());
}

/// At EL2 the same specifier is the guest virtual timer behind `VM_TMR_FIQ_ENA_EL2`: unmasked at map, masked by the ack,
/// unmasked again by the eoi.
#[test]
fn el2_guest_timer_is_masked_for_the_handler_and_unmasked_by_the_eoi() {
    let sim = SimAic::new(AIC3_INFO, V3_CAPS);
    let a = aic(V3_CAPS, true);
    let lines = LineTable::new(&a.geo);
    let aff = FiqAffinity::new();
    let sys = RefCell::new(SimSys::default());
    let mut shadow = cpu_start(
        &a,
        &lines,
        &aff,
        &mut sim.core(),
        &mut Shared(&sys),
        &mut Reports::default(),
        0,
        CPU_A,
    );
    assert_eq!(sys.borrow().get(Sysreg::VmTmrFiqEna), 0);

    let m = map(
        &a,
        &lines,
        &aff,
        &mut sim.core(),
        &mut Shared(&sys),
        &mut shadow,
        CPU_A,
        &[DT_AIC_FIQ, AIC_TMR_GUEST_VIRT, IRQ_TYPE_LEVEL_HIGH],
        9,
    )
    .unwrap();
    assert_eq!(m.hwirq, fiq_hwirq(FIQ_TMR_EL02_VIRT));
    assert_eq!(sys.borrow().get(Sysreg::VmTmrFiqEna), VM_TMR_FIQ_ENABLE_V);

    sys.borrow_mut()
        .regs
        .insert(Sysreg::CntvCtlEl02, ENABLED_FIRING);
    let mut enable_seen_by_handler = None;
    let mut delivered = Vec::new();
    {
        let mut d = Dispatch::new(&a, &lines, Shared(&sys), |hwirq, line| {
            enable_seen_by_handler = Some(sys.borrow().get(Sysreg::VmTmrFiqEna));
            delivered.push((hwirq, line));
        });
        a.handle_fiq(&mut sim.core(), &mut Shared(&sys), &mut d);
    }
    assert_eq!(delivered, vec![(fiq_hwirq(FIQ_TMR_EL02_VIRT), 9)]);
    assert_eq!(enable_seen_by_handler, Some(0));
    complete(
        &a,
        &lines,
        &mut sim.core(),
        &mut Shared(&sys),
        &shadow,
        m.hwirq,
    );
    assert_eq!(sys.borrow().get(Sysreg::VmTmrFiqEna), VM_TMR_FIQ_ENABLE_V);

    // Masked by the core (unmapped), the eoi leaves it masked.
    unmap(
        &a,
        &lines,
        &mut sim.core(),
        &mut Shared(&sys),
        &mut shadow,
        m.hwirq,
    );
    assert_eq!(sys.borrow().get(Sysreg::VmTmrFiqEna), 0);
    assert_eq!(shadow, FiqShadow(0));
    complete(
        &a,
        &lines,
        &mut sim.core(),
        &mut Shared(&sys),
        &shadow,
        m.hwirq,
    );
    assert_eq!(sys.borrow().get(Sysreg::VmTmrFiqEna), 0);
}

#[test]
fn an_unmapped_firing_timer_is_counted_not_delivered() {
    let sim = SimAic::new(AIC3_INFO, V3_CAPS);
    let a = aic(V3_CAPS, false);
    let lines = LineTable::new(&a.geo);
    let sys = RefCell::new(SimSys::with(&[(Sysreg::CntpCtlEl0, ENABLED_FIRING)]));
    let mut delivered = 0;
    let mut d = Dispatch::new(&a, &lines, Shared(&sys), |_, _| delivered += 1);
    a.handle_fiq(&mut sim.core(), &mut Shared(&sys), &mut d);
    assert_eq!(d.unmapped, 1);
    assert_eq!(delivered, 0);
}

/// v3 has fast IPIs: a pending IPI_SR is acknowledged and goes to the (unused) mux, before the timers are polled
/// (irq-apple-aic.c:574-576).
#[test]
fn a_fast_ipi_is_acknowledged_and_counted() {
    let sim = SimAic::new(AIC3_INFO, V3_CAPS);
    let a = aic(V3_CAPS, false);
    let lines = LineTable::new(&a.geo);
    let sys = RefCell::new(SimSys::with(&[(Sysreg::IpiSr, IPI_SR_PENDING)]));
    let mut d = Dispatch::new(&a, &lines, Shared(&sys), |_, _| {});
    a.handle_fiq(&mut sim.core(), &mut Shared(&sys), &mut d);
    assert_eq!((d.ipis, d.unmapped), (1, 0));
    assert_eq!(sys.borrow().get(Sysreg::IpiSr), 0);
}

// ---- hardware IRQs ----

#[test]
fn irq_is_delivered_masked_and_unmasked_by_the_eoi() {
    let sim = SimAic::new(AIC3_INFO, V3_CAPS);
    let a = aic(V3_CAPS, false);
    let lines = LineTable::new(&a.geo);
    let aff = FiqAffinity::new();
    let sys = RefCell::new(SimSys::default());
    let mut shadow = FiqShadow(0);
    a.init_hw(&mut sim.core());
    assert!(sim.masked(0, 40));

    let m = map(
        &a,
        &lines,
        &aff,
        &mut sim.core(),
        &mut Shared(&sys),
        &mut shadow,
        CPU_A,
        &[DT_AIC_IRQ, 40, IRQ_TYPE_LEVEL_HIGH],
        12,
    )
    .unwrap();
    assert!(!sim.masked(0, 40));

    sim.set_line(0, 40, true);
    sim.set_line(0, 41, true); // not mapped
    let mut delivered = Vec::new();
    let mut d = Dispatch::new(&a, &lines, Shared(&sys), |hwirq, line| {
        delivered.push((hwirq, line))
    });
    a.handle_irq(&mut sim.core(), &sim.event(), &mut Shared(&sys), &mut d);
    assert_eq!(
        d.unmapped, 0,
        "41 is masked since init_hw, so its event never comes"
    );
    assert_eq!(delivered, vec![(m.hwirq, 12)]);
    // The event read masked it; the eoi unmasks it.
    assert!(sim.masked(0, 40));
    complete(
        &a,
        &lines,
        &mut sim.core(),
        &mut Shared(&sys),
        &shadow,
        m.hwirq,
    );
    assert!(!sim.masked(0, 40));

    // Disabled by the core: the eoi of an event taken before leaves it masked.
    set_irq_enabled(&a, &lines, &mut sim.core(), m.hwirq, false);
    assert!(sim.masked(0, 40));
    complete(
        &a,
        &lines,
        &mut sim.core(),
        &mut Shared(&sys),
        &shadow,
        m.hwirq,
    );
    assert!(sim.masked(0, 40));
    set_irq_enabled(&a, &lines, &mut sim.core(), m.hwirq, true);
    assert!(!sim.masked(0, 40));

    // Unmapped: masked, and an eoi does not unmask it.
    unmap(
        &a,
        &lines,
        &mut sim.core(),
        &mut Shared(&sys),
        &mut shadow,
        m.hwirq,
    );
    assert!(sim.masked(0, 40));
    complete(
        &a,
        &lines,
        &mut sim.core(),
        &mut Shared(&sys),
        &shadow,
        m.hwirq,
    );
    assert!(sim.masked(0, 40));
}

#[test]
fn an_unmapped_irq_event_stays_masked() {
    let sim = SimAic::new(AIC3_INFO, V3_CAPS);
    let a = aic(V3_CAPS, false);
    let lines = LineTable::new(&a.geo);
    let sys = RefCell::new(SimSys::default());
    // Masks start clear in the model; nothing is mapped.
    sim.set_line(0, 3, true);
    let mut d = Dispatch::new(&a, &lines, Shared(&sys), |_, _| {});
    a.handle_irq(&mut sim.core(), &sim.event(), &mut Shared(&sys), &mut d);
    assert_eq!(d.unmapped, 1);
    assert!(sim.masked(0, 3));
}

// ---- per-CPU start ----

/// A FIQ mapped on one CPU is unmasked on another by that CPU's start, if its affinity includes it.
#[test]
fn cpu_start_unmasks_mapped_fiqs_within_their_affinity() {
    let sim = SimAic::new(AIC3_INFO, V3_CAPS);
    let a = aic(V3_CAPS, false);
    let lines = LineTable::new(&a.geo);
    let mut aff = FiqAffinity::new();
    aff.add_child(Some(AIC_CPU_PMU_P), Some(&[10]), resolve);

    let sys_a = RefCell::new(SimSys::default());
    let mut shadow_a = cpu_start(
        &a,
        &lines,
        &aff,
        &mut sim.core(),
        &mut Shared(&sys_a),
        &mut Reports::default(),
        0,
        CPU_A,
    );
    let timer = map(
        &a,
        &lines,
        &aff,
        &mut sim.core(),
        &mut Shared(&sys_a),
        &mut shadow_a,
        CPU_A,
        &[DT_AIC_FIQ, AIC_TMR_GUEST_VIRT, IRQ_TYPE_LEVEL_HIGH],
        1,
    )
    .unwrap();
    let pmu = map(
        &a,
        &lines,
        &aff,
        &mut sim.core(),
        &mut Shared(&sys_a),
        &mut shadow_a,
        CPU_A,
        &[DT_AIC_FIQ, AIC_CPU_PMU_P, IRQ_TYPE_LEVEL_HIGH],
        2,
    )
    .unwrap();
    assert_eq!(
        shadow_a.0,
        (1 << hwirq_irq(timer.hwirq)) | (1 << hwirq_irq(pmu.hwirq))
    );

    // CPU B is outside the P-core PMU's set.
    let sys_b = RefCell::new(SimSys::default());
    let shadow_b = cpu_start(
        &a,
        &lines,
        &aff,
        &mut sim.core(),
        &mut Shared(&sys_b),
        &mut Reports::default(),
        1,
        CPU_B,
    );
    assert_eq!(shadow_b.0, 1 << hwirq_irq(timer.hwirq));
    // Its own start masked its timers, as the boot CPU's did.
    assert_eq!(
        sys_b.borrow().get(Sysreg::CntvCtlEl0) & ARCH_TIMER_CTRL_IT_MASK,
        ARCH_TIMER_CTRL_IT_MASK
    );
}

/// v1 (`apple,aic`) checks WHOAMI at each CPU's start (irq-apple-aic.c:900); the report reaches the handler.
#[test]
fn v1_cpu_start_reports_a_whoami_mismatch() {
    let sim = SimAic::new(AIC1_INFO, Caps::V1 { aic_info: 896 });
    let a = Aic {
        info: AIC1_INFO,
        geo: derive_geometry(&AIC1_INFO, Caps::V1 { aic_info: 896 }).unwrap(),
        el2: false,
    };
    let lines = LineTable::new(&a.geo);
    let aff = FiqAffinity::new();
    let sys = RefCell::new(SimSys::default());
    let mut reports = Reports::default();
    cpu_start(
        &a,
        &lines,
        &aff,
        &mut sim.core(),
        &mut Shared(&sys),
        &mut reports,
        2,
        CPU_A,
    );
    assert_eq!(
        reports.0,
        vec![Report::WhoamiMismatch { whoami: 0, cpu: 2 }]
    );
}
