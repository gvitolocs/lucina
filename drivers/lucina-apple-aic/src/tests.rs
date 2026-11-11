// SPDX-License-Identifier: GPL-2.0-or-later
// Derived from drivers/irqchip/irq-apple-aic.c of AsahiLinux/linux (branch asahi, rev 6ba0637d1ed9):
// Copyright The Asahi Linux Contributors; based on irq-lpc32xx (Copyright 2015-2016 Vladimir Zapolskiy) and
// irq-bcm2836 (Copyright 2015 Broadcom). Behaviour re-expressed in Rust for LucinaOS; see the crate README.
// This is a Rust port of the behaviour of drivers/irqchip/irq-apple-aic.c; the original is the source of truth.

//! Tests against the C's behaviour. Hardware numbers the C reads from the AIC (`nr_irq`, `max_irq`, die counts) are inputs:
//! the only one the C states is "896 level-triggered hardware IRQs" (irq-apple-aic.c:14), used for the v1 (M1) shape. The
//! v2/v3 register contents below are synthetic, chosen to exercise the formulas, and claim nothing about any Mac.

use std::vec;
use std::vec::Vec;

use crate::chip::{AffinityError, Aic, FiqShadow};
use crate::fwspec::*;
use crate::hwirq::*;
use crate::info::*;
use crate::ipi_mux::{IpiMuxCpu, for_each_ipi};
use crate::mmio::{AicMmio, Report, Sysreg};
use crate::regs::*;
use crate::sim::{Ev, Rec, SimAic, SimSys, SysOp};

/// v1 with the C's 896 IRQs (irq-apple-aic.c:14).
const M1_CAPS: Caps = Caps::V1 { aic_info: 896 };
/// Synthetic v2/v3 caps: 600 IRQs of 640, two dies (LAST_DIE 1), MAX_DIE 2.
const V2_CAPS: Caps = Caps::V2 {
    info1: (1 << 24) | 600,
    info3: (2 << 24) | 640,
};
/// Synthetic single-die v3 caps: 512 IRQs of 1024.
const V3_CAPS: Caps = Caps::V2 {
    info1: 512,
    info3: 1024,
};

fn aic(info: AicInfo, caps: Caps, el2: bool) -> Aic {
    Aic {
        info,
        geo: derive_geometry(&info, caps).unwrap(),
        el2,
    }
}

const M4_COMPATIBLE: [&str; 2] = ["apple,t8132-aic3", "apple,t8122-aic3"];

// ---- info ----

#[test]
fn compatibles_match_as_the_table() {
    assert_eq!(
        match_compatible(&["apple,t8103-aic", "apple,aic"]),
        Some(AIC1_LOCAL_FIPI_INFO)
    );
    assert_eq!(
        match_compatible(&["apple,t8015-aic", "apple,aic"]),
        Some(AIC1_FIPI_INFO)
    );
    assert_eq!(match_compatible(&["apple,aic"]), Some(AIC1_INFO));
    assert_eq!(
        match_compatible(&["apple,t6000-aic", "apple,aic2"]),
        Some(AIC2_INFO)
    );
    assert_eq!(match_compatible(&["apple,t8122-aic3"]), Some(AIC3_INFO));
    // The node's first known string wins (of_match_node's score).
    assert_eq!(
        match_compatible(&["apple,aic", "apple,t8103-aic"]),
        Some(AIC1_INFO)
    );
    assert_eq!(match_compatible(&["apple,gic"]), None);
}

#[test]
fn the_m4_node_is_an_aic3_through_its_t8122_fallback() {
    let info = match_compatible(&M4_COMPATIBLE).unwrap();
    assert_eq!(info, AIC3_INFO);
    assert_eq!(info.version, Version::V3);
    assert_eq!(info.irq_cfg, 0x10000);
    assert!(info.fast_ipi && info.local_fast_ipi);
    assert_eq!(info.target_cpu, 0);
    assert_eq!(info.event, 0);
    assert!(is_declared(&M4_COMPATIBLE));
    // t8132 alone is in neither the match table nor IRQCHIP_DECLARE.
    assert_eq!(match_compatible(&["apple,t8132-aic3"]), None);
    assert!(!is_declared(&["apple,t8132-aic3"]));
}

#[test]
fn v2_and_v3_differ_only_in_the_block_base() {
    assert_eq!(AIC2_INFO.irq_cfg, 0x2000);
    assert_eq!(
        AicInfo {
            version: Version::V2,
            irq_cfg: 0x2000,
            ..AIC3_INFO
        },
        AIC2_INFO
    );
    let g2 = derive_geometry(&AIC2_INFO, V2_CAPS).unwrap();
    let g3 = derive_geometry(&AIC3_INFO, V2_CAPS).unwrap();
    let d = 0x10000 - 0x2000;
    assert_eq!(g3.sw_set - g2.sw_set, d);
    assert_eq!(g3.mask_clr - g2.mask_clr, d);
    assert_eq!(g3.die_stride, g2.die_stride);
}

// ---- geometry ----

#[test]
fn m1_geometry() {
    let g = derive_geometry(&AIC1_LOCAL_FIPI_INFO, M1_CAPS).unwrap();
    assert_eq!(
        (g.nr_irq, g.max_irq, g.nr_die, g.max_die),
        (896, 0x400, 1, 1)
    );
    // TARGET_CPU at 0x3000, 0x400 words; then five arrays of 0x400 / 32 = 32 words (128 bytes).
    assert_eq!(g.start_off, 0x3000);
    assert_eq!(g.sw_set, 0x4000);
    assert_eq!(g.sw_clr, 0x4080);
    assert_eq!(g.mask_set, 0x4100);
    assert_eq!(g.mask_clr, 0x4180);
    assert_eq!(g.hw_state, 0x4200);
    assert_eq!(g.die_stride, 0x1280);
    assert!(g.event_in_core);
}

#[test]
fn v1_takes_only_the_low_16_bits_of_aic_info() {
    let g = derive_geometry(
        &AIC1_INFO,
        Caps::V1 {
            aic_info: 0xabcd_0380,
        },
    )
    .unwrap();
    assert_eq!(g.nr_irq, 0x380);
}

#[test]
fn v2_geometry_follows_the_capability_registers() {
    let g = derive_geometry(&AIC2_INFO, V2_CAPS).unwrap();
    assert_eq!((g.nr_irq, g.max_irq, g.nr_die, g.max_die), (600, 640, 2, 2));
    let words = 4 * (640 >> 5);
    assert_eq!(g.start_off, 0x2000);
    assert_eq!(g.sw_set, 0x2000 + 4 * 640);
    assert_eq!(g.sw_clr, g.sw_set + words);
    assert_eq!(g.mask_set, g.sw_clr + words);
    assert_eq!(g.mask_clr, g.mask_set + words);
    assert_eq!(g.hw_state, g.mask_clr + words);
    assert_eq!(g.die_stride, 4 * 640 + 5 * words);
    assert!(!g.event_in_core);
}

#[test]
fn geometry_formula_holds_for_every_max_irq() {
    for max_irq in (0..=0xffffu32).step_by(97) {
        for last_die in 0..16u32 {
            let caps = Caps::V2 {
                info1: (last_die << 24) | 0x1234,
                info3: (0xf << 24) | max_irq,
            };
            let g = derive_geometry(&AIC3_INFO, caps).unwrap();
            let w = 4 * (max_irq >> 5);
            assert_eq!(g.nr_die, last_die + 1);
            assert_eq!(g.max_die, 0xf);
            assert_eq!(g.sw_set, 0x10000 + 4 * max_irq);
            assert_eq!(
                [g.sw_clr, g.mask_set, g.mask_clr, g.hw_state],
                [
                    g.sw_set + w,
                    g.sw_set + 2 * w,
                    g.sw_set + 3 * w,
                    g.sw_set + 4 * w
                ]
            );
            assert_eq!(g.die_stride, 4 * max_irq + 5 * w);
        }
    }
}

#[test]
fn caps_of_the_wrong_generation_are_refused() {
    assert_eq!(derive_geometry(&AIC1_INFO, V2_CAPS), None);
    assert_eq!(derive_geometry(&AIC3_INFO, M1_CAPS), None);
}

#[test]
fn caps_are_read_from_the_registers_the_c_reads() {
    let sim = SimAic::new(AIC3_INFO, V3_CAPS);
    assert_eq!(read_caps(&AIC3_INFO, &sim.core()), V3_CAPS);
    let sim1 = SimAic::new(AIC1_INFO, M1_CAPS);
    assert_eq!(read_caps(&AIC1_INFO, &sim1.core()), M1_CAPS);
}

// ---- hwirq ----

#[test]
fn hwirq_fields_round_trip() {
    for die in 0..256u32 {
        for irq in (0..65536u32).step_by(251).chain([0xffff]) {
            let h = irq_hwirq(die, irq);
            assert_eq!(
                (hwirq_die(h), hwirq_type(h), hwirq_irq(h)),
                (die, AIC_EVENT_TYPE_IRQ, irq)
            );
            assert_eq!(repack(h), h);
            assert_eq!(
                h,
                field_prep(AIC_EVENT_DIE, die)
                    | field_prep(AIC_EVENT_TYPE, 1)
                    | field_prep(AIC_EVENT_NUM, irq)
            );
        }
    }
    for x in 0..65536u32 {
        let h = fiq_hwirq(x);
        assert_eq!(
            (hwirq_die(h), hwirq_type(h), hwirq_irq(h)),
            (0, AIC_EVENT_TYPE_FIQ, x)
        );
        assert_eq!(repack(h), h);
    }
}

#[test]
fn hwirq_fields_are_masked_like_field_prep() {
    assert_eq!(irq_hwirq(0x1ff, 0x1_0005), irq_hwirq(0xff, 5));
    assert_eq!(fiq_hwirq(0x7_0003), 3);
    // Arbitrary words repack to themselves too.
    let mut x = 0x1234_5678u32;
    for _ in 0..10_000 {
        x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        assert_eq!(repack(x), x);
    }
}

#[test]
fn hwirq_layout_matches_the_event_register_constants() {
    assert_eq!(AIC_EVENT_DIE, 0xff00_0000);
    assert_eq!(AIC_EVENT_TYPE, 0x00ff_0000);
    assert_eq!(AIC_EVENT_NUM, 0x0000_ffff);
    assert_eq!(irq_hwirq(3, 7), 0x0301_0007);
}

// ---- translate ----

#[test]
fn three_and_four_cell_irqs() {
    let a = aic(AIC2_INFO, V2_CAPS, true);
    assert_eq!(
        a.translate(&[0, 17, 4]),
        Ok(Translated {
            hwirq: irq_hwirq(0, 17),
            sense: 4
        })
    );
    assert_eq!(
        a.translate(&[0, 1, 599, 1]),
        Ok(Translated {
            hwirq: irq_hwirq(1, 599),
            sense: 1
        })
    );
    // die >= nr_die, irq >= nr_irq
    assert_eq!(a.translate(&[0, 2, 0, 4]), Err(TranslateError::Invalid));
    assert_eq!(a.translate(&[0, 600, 4]), Err(TranslateError::Invalid));
    // cell counts, unknown first cell
    assert_eq!(a.translate(&[0, 1]), Err(TranslateError::Invalid));
    assert_eq!(a.translate(&[0, 0, 1, 4, 0]), Err(TranslateError::Invalid));
    assert_eq!(a.translate(&[2, 1, 4]), Err(TranslateError::Invalid));
    // the sense mask
    assert_eq!(a.translate(&[0, 5, 0xff4]).unwrap().sense, 4);
}

#[test]
fn fiqs_at_el2_are_their_binding_index_but_the_e_pmu_is_merged() {
    let a = aic(AIC3_INFO, V3_CAPS, true);
    for (idx, hw) in [(0, 0), (1, 1), (2, 2), (3, 3), (4, 5), (5, 5), (6, 6)] {
        assert_eq!(
            a.translate(&[1, idx, 4]).unwrap().hwirq,
            fiq_hwirq(hw),
            "index {idx}"
        );
    }
    assert_eq!(a.translate(&[1, 7, 4]), Err(TranslateError::Invalid));
    // A 4-cell FIQ must name die 0.
    assert_eq!(a.translate(&[1, 0, 3, 4]).unwrap().hwirq, fiq_hwirq(3));
    assert_eq!(a.translate(&[1, 1, 3, 4]), Err(TranslateError::Invalid));
}

#[test]
fn fiqs_at_el1_remap_the_timers() {
    let a = aic(AIC3_INFO, V3_CAPS, false);
    assert_eq!(
        a.translate(&[1, AIC_TMR_GUEST_PHYS, 4]).unwrap().hwirq,
        fiq_hwirq(FIQ_TMR_EL0_PHYS)
    );
    assert_eq!(
        a.translate(&[1, AIC_TMR_GUEST_VIRT, 4]).unwrap().hwirq,
        fiq_hwirq(FIQ_TMR_EL0_VIRT)
    );
    assert_eq!(
        a.translate(&[1, AIC_TMR_HV_PHYS, 4]),
        Err(TranslateError::NoEntry)
    );
    assert_eq!(
        a.translate(&[1, AIC_TMR_HV_VIRT, 4]),
        Err(TranslateError::NoEntry)
    );
    assert_eq!(
        a.translate(&[1, AIC_CPU_PMU_E, 4]).unwrap().hwirq,
        fiq_hwirq(AIC_CPU_PMU_P)
    );
    assert_eq!(
        a.translate(&[1, FIQ_VGIC_MI, 4]).unwrap().hwirq,
        fiq_hwirq(FIQ_VGIC_MI)
    );
}

#[test]
fn every_valid_specifier_round_trips_through_its_canonical_cells() {
    for el2 in [false, true] {
        let a = aic(AIC2_INFO, V2_CAPS, el2);
        let mut specs: Vec<Vec<u32>> = Vec::new();
        for die in 0..3 {
            for irq in (0..700).step_by(7) {
                specs.push(vec![0, die, irq, 4]);
                specs.push(vec![0, irq, 1]);
            }
        }
        for idx in 0..9 {
            specs.push(vec![1, idx, 4]);
            specs.push(vec![1, 0, idx, 4]);
        }
        for s in specs {
            let Ok(t) = a.translate(&s) else { continue };
            let (cells, n) = to_cells(t.hwirq, t.sense, el2).expect("a translated hwirq has cells");
            assert_eq!(a.translate(&cells[..n]), Ok(t), "{s:?} at el2={el2}");
        }
    }
}

#[test]
fn every_reachable_hwirq_round_trips_through_to_cells() {
    for el2 in [false, true] {
        let a = aic(AIC2_INFO, V2_CAPS, el2);
        for die in 0..2 {
            for irq in 0..600 {
                let h = irq_hwirq(die, irq);
                let (c, n) = to_cells(h, 4, el2).unwrap();
                assert_eq!(n, if die == 0 { 3 } else { 4 });
                assert_eq!(a.translate(&c[..n]).unwrap().hwirq, h);
            }
        }
        for x in 0..AIC_NR_FIQ + 2 {
            let h = fiq_hwirq(x);
            match to_cells(h, 4, el2) {
                Some((c, n)) => assert_eq!(a.translate(&c[..n]).unwrap().hwirq, h),
                None => {
                    let unreachable = x >= AIC_NR_FIQ
                        || x == AIC_CPU_PMU_E
                        || (!el2 && (x == FIQ_TMR_EL02_PHYS || x == FIQ_TMR_EL02_VIRT));
                    assert!(unreachable, "fiq {x} at el2={el2}");
                }
            }
        }
    }
}

#[test]
fn decode_names_the_shapes() {
    assert_eq!(decode(irq_hwirq(1, 9)), Decoded::Irq { die: 1, num: 9 });
    assert_eq!(decode(fiq_hwirq(5)), Decoded::Fiq { num: 5 });
    assert_eq!(
        decode(0x0004_0001),
        Decoded::Other {
            die: 0,
            ty: 4,
            num: 1
        }
    );
}

#[test]
fn fiq_affinity_uses_the_binding_index() {
    assert_eq!(fiq_affinity_index(&[0, 3, 4]), None);
    assert_eq!(
        fiq_affinity_index(&[1, AIC_CPU_PMU_E, 4]),
        Some(AIC_CPU_PMU_E)
    );
    assert_eq!(
        fiq_affinity_index(&[1, 0, AIC_CPU_PMU_P, 4]),
        Some(AIC_CPU_PMU_P)
    );
    assert_eq!(fiq_affinity_index(&[1, 9, 4]), None);
}

#[test]
fn chips_and_trigger_types() {
    assert_eq!(chip_for(Version::V1, irq_hwirq(0, 3)), ChipKind::Aic);
    assert_eq!(chip_for(Version::V3, irq_hwirq(0, 3)), ChipKind::Aic2);
    assert_eq!(chip_for(Version::V2, fiq_hwirq(0)), ChipKind::Fiq);
    assert_eq!(irq_set_type(IRQ_TYPE_LEVEL_HIGH), Ok(()));
    assert_eq!(irq_set_type(IRQ_TYPE_EDGE_RISING), Ok(()));
    assert!(irq_set_type(2).is_err() && irq_set_type(8).is_err() && irq_set_type(0).is_err());
    assert_eq!(fiq_set_type(IRQ_TYPE_LEVEL_HIGH), Ok(()));
    assert!(fiq_set_type(IRQ_TYPE_EDGE_RISING).is_err());
}

// ---- mask, unmask, eoi, affinity ----

#[test]
fn masking_a_hwirq_sets_its_bit_in_its_dies_mask_set() {
    let a = aic(AIC2_INFO, V2_CAPS, true);
    let sim = SimAic::new(AIC2_INFO, V2_CAPS);
    let mut core = sim.core();
    for (die, irq) in [(0, 0), (0, 31), (0, 32), (1, 0), (1, 77), (1, 599)] {
        sim.clear_log();
        let h = irq_hwirq(die, irq);
        a.irq_mask(&mut core, h);
        let off = (a.geo.mask_set + die * a.geo.die_stride + 4 * (irq >> 5)) as usize;
        assert_eq!(sim.writes(), vec![(off, 1 << (irq & 31))]);
        assert!(sim.masked(die, irq));
        a.irq_unmask(&mut core, h);
        let off = (a.geo.mask_clr + die * a.geo.die_stride + 4 * (irq >> 5)) as usize;
        assert_eq!(sim.writes()[1], (off, 1 << (irq & 31)));
        assert!(!sim.masked(die, irq));
    }
}

#[test]
fn eoi_unmasks_unless_the_core_masked_it() {
    let a = aic(AIC3_INFO, V3_CAPS, true);
    let sim = SimAic::new(AIC3_INFO, V3_CAPS);
    let mut core = sim.core();
    a.irq_mask(&mut core, irq_hwirq(0, 40));
    a.irq_eoi(&mut core, irq_hwirq(0, 40), true);
    assert!(sim.masked(0, 40));
    a.irq_eoi(&mut core, irq_hwirq(0, 40), false);
    assert!(!sim.masked(0, 40));
}

#[test]
fn affinity_is_v1_only() {
    let a = aic(AIC1_INFO, M1_CAPS, true);
    let sim = SimAic::new(AIC1_INFO, M1_CAPS);
    let mut core = sim.core();
    assert_eq!(
        a.irq_set_affinity(&mut core, irq_hwirq(0, 10), 0b1100, 0b1011, false),
        Ok(3)
    );
    assert_eq!(sim.writes(), vec![(0x3000 + 40, 1 << 3)]);
    assert_eq!(
        a.irq_set_affinity(&mut core, irq_hwirq(0, 10), 0b1100, 0b1011, true),
        Ok(2)
    );
    assert_eq!(
        a.irq_set_affinity(&mut core, irq_hwirq(0, 10), 0b0100, 0b1011, false),
        Err(AffinityError::NoCpu)
    );
    let v3 = aic(AIC3_INFO, V3_CAPS, true);
    assert_eq!(
        v3.irq_set_affinity(&mut core, irq_hwirq(0, 1), 1, 1, true),
        Err(AffinityError::NotSupported)
    );
}

// ---- IRQ entry ----

#[test]
fn the_event_drain_ends_while_lines_stay_asserted() {
    let a = aic(AIC2_INFO, V2_CAPS, true);
    let sim = SimAic::new(AIC2_INFO, V2_CAPS);
    let (mut core, event) = (sim.core(), sim.event());
    a.init_hw(&mut core);
    for (d, n) in [(0, 300), (1, 5), (0, 2)] {
        sim.set_line(d, n, true);
        a.irq_unmask(&mut core, irq_hwirq(d, n));
    }
    let (mut sys, mut rec) = (SimSys::default(), Rec::default());
    a.handle_irq(&mut core, &event, &mut sys, &mut rec);
    // Lowest number first, die 0 before die 1 in the model; each once, though the lines stay up.
    assert_eq!(
        rec.hwirqs(),
        vec![irq_hwirq(0, 2), irq_hwirq(0, 300), irq_hwirq(1, 5)]
    );
    // A second entry finds nothing: the event read masked them.
    let mut rec2 = Rec::default();
    a.handle_irq(&mut core, &event, &mut sys, &mut rec2);
    assert!(rec2.events.is_empty());
    // The eoi of one unmasks it, and the still-asserted line comes back.
    a.irq_eoi(&mut core, irq_hwirq(0, 300), false);
    let mut rec3 = Rec::default();
    a.handle_irq(&mut core, &event, &mut sys, &mut rec3);
    assert_eq!(rec3.hwirqs(), vec![irq_hwirq(0, 300)]);
}

#[test]
fn a_software_trigger_is_ored_with_the_line_and_cleared_by_init() {
    let a = aic(AIC3_INFO, V3_CAPS, true);
    let sim = SimAic::new(AIC3_INFO, V3_CAPS);
    let (mut core, event) = (sim.core(), sim.event());
    let (off, bit) = a.bit_reg(a.geo.sw_set, irq_hwirq(0, 70));
    core.write32(off, bit);
    a.init_hw(&mut core);
    a.irq_unmask(&mut core, irq_hwirq(0, 70));
    let (mut sys, mut rec) = (SimSys::default(), Rec::default());
    a.handle_irq(&mut core, &event, &mut sys, &mut rec);
    assert!(rec.events.is_empty());
    core.write32(off, bit);
    a.handle_irq(&mut core, &event, &mut sys, &mut rec);
    assert_eq!(rec.hwirqs(), vec![irq_hwirq(0, 70)]);
}

#[test]
fn unknown_events_are_reported_and_the_self_ipi_is_not_handled() {
    let a = aic(AIC1_INFO, M1_CAPS, false);
    let sim = SimAic::new(AIC1_INFO, M1_CAPS);
    let (mut core, event) = (sim.core(), sim.event());
    sim.st.borrow_mut().ipi_pending = AIC_IPI_SELF;
    let (mut sys, mut rec) = (SimSys::default(), Rec::default());
    a.handle_irq(&mut core, &event, &mut sys, &mut rec);
    assert_eq!(
        rec.events,
        vec![Ev::Report(Report::UnknownEvent {
            ty: AIC_EVENT_TYPE_IPI,
            num: AIC_EVENT_IPI_SELF
        })]
    );
}

#[test]
fn v1_ipi_without_fast_ipis_acks_processes_and_unmasks() {
    let a = aic(AIC1_INFO, M1_CAPS, false);
    let sim = SimAic::new(AIC1_INFO, M1_CAPS);
    let (mut core, event) = (sim.core(), sim.event());
    let (mut sys, mut rec) = (SimSys::default(), Rec::default());
    a.init_cpu(&mut core, &mut sys, &mut rec, 0);
    sim.clear_log();
    sys.log.clear();
    a.ipi_send_single(&mut core, &mut sys, 0, 0);
    assert_eq!(sim.writes(), vec![(AIC_IPI_SEND as usize, 1)]);
    sim.clear_log();
    a.handle_irq(&mut core, &event, &mut sys, &mut rec);
    assert_eq!(rec.events, vec![Ev::Mux]);
    assert_eq!(
        sim.writes(),
        vec![
            (AIC_IPI_ACK as usize, AIC_IPI_OTHER),
            (AIC_IPI_MASK_CLR as usize, AIC_IPI_OTHER)
        ]
    );
    assert_eq!(sim.st.borrow().ipi_mask & AIC_IPI_OTHER, 0);
    assert!(sys.log.is_empty());
}

#[test]
fn the_vgic_maintenance_interrupt_is_handled_then_disarmed() {
    let a = aic(AIC3_INFO, V3_CAPS, true);
    let sim = SimAic::new(AIC3_INFO, V3_CAPS);
    let (mut core, event) = (sim.core(), sim.event());
    let mut sys = SimSys::with(&[
        (Sysreg::IchHcrEl2, ICH_HCR_EL2_EN | 0x10),
        (Sysreg::IchMisrEl2, 0x4),
    ]);
    let mut rec = Rec::default();
    a.handle_irq(&mut core, &event, &mut sys, &mut rec);
    assert_eq!(
        rec.events,
        vec![
            Ev::Hwirq(fiq_hwirq(FIQ_VGIC_MI)),
            Ev::Report(Report::VgicNotHandled { misr: 0x4 })
        ]
    );
    assert_eq!(sys.get(Sysreg::IchHcrEl2), 0x10);
    // At EL1 the registers are not touched.
    let el1 = aic(AIC3_INFO, V3_CAPS, false);
    let mut sys1 = SimSys::with(&[(Sysreg::IchHcrEl2, 1), (Sysreg::IchMisrEl2, 1)]);
    el1.handle_irq(&mut core, &event, &mut sys1, &mut Rec::default());
    assert!(sys1.log.is_empty());
}

// ---- FIQ entry ----

const FIRING: u64 = ARCH_TIMER_CTRL_ENABLE | ARCH_TIMER_CTRL_IT_STAT;

#[test]
fn timer_firing_is_enabled_unmasked_and_met() {
    assert!(timer_firing(FIRING));
    assert!(timer_firing(FIRING | 0x100));
    assert!(!timer_firing(FIRING | ARCH_TIMER_CTRL_IT_MASK));
    assert!(!timer_firing(ARCH_TIMER_CTRL_IT_STAT));
    assert!(!timer_firing(ARCH_TIMER_CTRL_ENABLE));
}

#[test]
fn fiq_polling_reads_every_source_in_the_c_order() {
    let a = aic(AIC3_INFO, V3_CAPS, true);
    let sim = SimAic::new(AIC3_INFO, V3_CAPS);
    let mut core = sim.core();
    // Nothing pending, guest timers enabled: every source is read, in order.
    let mut sys = SimSys::with(&[(
        Sysreg::VmTmrFiqEna,
        VM_TMR_FIQ_ENABLE_P | VM_TMR_FIQ_ENABLE_V,
    )]);
    let mut rec = Rec::default();
    a.handle_fiq(&mut core, &mut sys, &mut rec);
    assert!(rec.events.is_empty());
    assert_eq!(
        sys.reads(),
        vec![
            Sysreg::IpiSr,
            Sysreg::CntpCtlEl0,
            Sysreg::CntvCtlEl0,
            Sysreg::VmTmrFiqEna,
            Sysreg::CntpCtlEl02,
            Sysreg::CntvCtlEl02,
            Sysreg::Pmcr0,
            Sysreg::Upmcr0,
        ]
    );
    // Guest timers disabled: their control registers are not read.
    let mut sys = SimSys::default();
    a.handle_fiq(&mut core, &mut sys, &mut rec);
    assert_eq!(
        sys.reads(),
        vec![
            Sysreg::IpiSr,
            Sysreg::CntpCtlEl0,
            Sysreg::CntvCtlEl0,
            Sysreg::VmTmrFiqEna,
            Sysreg::Pmcr0,
            Sysreg::Upmcr0,
        ]
    );
    // At EL1 there are no guest timers.
    let el1 = aic(AIC3_INFO, V3_CAPS, false);
    let mut sys = SimSys::default();
    el1.handle_fiq(&mut core, &mut sys, &mut rec);
    assert_eq!(
        sys.reads(),
        vec![
            Sysreg::IpiSr,
            Sysreg::CntpCtlEl0,
            Sysreg::CntvCtlEl0,
            Sysreg::Pmcr0,
            Sysreg::Upmcr0
        ]
    );
    // Without fast IPIs (v1 apple,aic): neither IPI_SR nor the uncore PMC.
    let v1 = aic(AIC1_INFO, M1_CAPS, true);
    let sim1 = SimAic::new(AIC1_INFO, M1_CAPS);
    let mut sys = SimSys::default();
    v1.handle_fiq(&mut sim1.core(), &mut sys, &mut rec);
    assert_eq!(
        sys.reads(),
        vec![
            Sysreg::CntpCtlEl0,
            Sysreg::CntvCtlEl0,
            Sysreg::VmTmrFiqEna,
            Sysreg::Pmcr0
        ]
    );
    assert!(rec.events.is_empty());
}

#[test]
fn every_fiq_source_dispatches_in_order() {
    let a = aic(AIC3_INFO, V3_CAPS, true);
    let sim = SimAic::new(AIC3_INFO, V3_CAPS);
    let mut core = sim.core();
    let mut sys = SimSys::with(&[
        (Sysreg::IpiSr, IPI_SR_PENDING),
        (Sysreg::CntpCtlEl0, FIRING),
        (Sysreg::CntvCtlEl0, FIRING),
        (
            Sysreg::VmTmrFiqEna,
            VM_TMR_FIQ_ENABLE_P | VM_TMR_FIQ_ENABLE_V,
        ),
        (Sysreg::CntpCtlEl02, FIRING),
        (Sysreg::CntvCtlEl02, FIRING),
        (Sysreg::Pmcr0, (PMCR0_IMODE_FIQ << 8) | PMCR0_IACT | 0xff),
        (Sysreg::Upmcr0, (UPMCR0_IMODE_FIQ << 16) | 0x1),
        (Sysreg::Upmsr, UPMSR_IACT),
    ]);
    let mut rec = Rec::default();
    a.handle_fiq(&mut core, &mut sys, &mut rec);
    assert_eq!(
        rec.events,
        vec![
            Ev::Mux,
            Ev::Hwirq(fiq_hwirq(FIQ_TMR_EL0_PHYS)),
            Ev::Hwirq(fiq_hwirq(FIQ_TMR_EL0_VIRT)),
            Ev::Hwirq(fiq_hwirq(FIQ_TMR_EL02_PHYS)),
            Ev::Hwirq(fiq_hwirq(FIQ_TMR_EL02_VIRT)),
            Ev::Hwirq(fiq_hwirq(AIC_CPU_PMU_P)),
            Ev::Report(Report::UncorePmcMasked),
        ]
    );
    // The fast IPI was acknowledged before the mux ran; the uncore PMC is now off, its other bits kept.
    assert_eq!(sys.get(Sysreg::IpiSr), 0);
    assert_eq!(sys.get(Sysreg::Upmcr0), 0x1);
    let ack = sys
        .log
        .iter()
        .position(|op| *op == SysOp::Write(Sysreg::IpiSr, IPI_SR_PENDING))
        .unwrap();
    assert_eq!(sys.log[ack + 1], SysOp::Isb);
}

#[test]
fn the_pmu_fiq_needs_fiq_mode_and_iact() {
    let a = aic(AIC3_INFO, V3_CAPS, true);
    let sim = SimAic::new(AIC3_INFO, V3_CAPS);
    for (pmcr0, fires) in [
        ((PMCR0_IMODE_FIQ << 8) | PMCR0_IACT, true),
        (PMCR0_IMODE_FIQ << 8, false),
        ((2 << 8) | PMCR0_IACT, false),
        (PMCR0_IACT, false),
    ] {
        let mut sys = SimSys::with(&[(Sysreg::Pmcr0, pmcr0)]);
        let mut rec = Rec::default();
        a.handle_fiq(&mut sim.core(), &mut sys, &mut rec);
        assert_eq!(
            rec.hwirqs() == vec![fiq_hwirq(AIC_CPU_PMU_P)],
            fires,
            "{pmcr0:#x}"
        );
    }
}

// ---- FIQ chip ----

#[test]
fn only_guest_timers_have_fiq_mask_bits() {
    let a = aic(AIC3_INFO, V3_CAPS, true);
    let mut sys = SimSys::with(&[(
        Sysreg::VmTmrFiqEna,
        VM_TMR_FIQ_ENABLE_P | VM_TMR_FIQ_ENABLE_V,
    )]);
    let mut shadow = FiqShadow(0);
    a.fiq_unmask(&mut sys, &mut shadow, fiq_hwirq(FIQ_TMR_EL02_PHYS));
    a.fiq_unmask(&mut sys, &mut shadow, fiq_hwirq(FIQ_TMR_EL0_PHYS));
    assert_eq!(shadow.0, 0b101);
    // The ack masks the guest physical timer; the eoi unmasks it because the core has it unmasked.
    sys.log.clear();
    a.fiq_set_mask(&mut sys, fiq_hwirq(FIQ_TMR_EL02_PHYS));
    assert_eq!(sys.get(Sysreg::VmTmrFiqEna), VM_TMR_FIQ_ENABLE_V);
    assert_eq!(
        sys.log,
        vec![
            SysOp::Read(Sysreg::VmTmrFiqEna),
            SysOp::Write(Sysreg::VmTmrFiqEna, VM_TMR_FIQ_ENABLE_V),
            SysOp::Isb
        ]
    );
    a.fiq_eoi(&mut sys, &shadow, fiq_hwirq(FIQ_TMR_EL02_PHYS));
    assert_eq!(
        sys.get(Sysreg::VmTmrFiqEna),
        VM_TMR_FIQ_ENABLE_V | VM_TMR_FIQ_ENABLE_P
    );
    // Masked by the core: the eoi leaves the mask on.
    a.fiq_mask(&mut sys, &mut shadow, fiq_hwirq(FIQ_TMR_EL02_VIRT));
    assert_eq!(sys.get(Sysreg::VmTmrFiqEna), VM_TMR_FIQ_ENABLE_P);
    a.fiq_eoi(&mut sys, &shadow, fiq_hwirq(FIQ_TMR_EL02_VIRT));
    assert_eq!(sys.get(Sysreg::VmTmrFiqEna), VM_TMR_FIQ_ENABLE_P);
    // The EL0 timers, the PMU and the vGIC FIQ have no mask register at all.
    sys.log.clear();
    for x in [
        FIQ_TMR_EL0_PHYS,
        FIQ_TMR_EL0_VIRT,
        AIC_CPU_PMU_P,
        FIQ_VGIC_MI,
    ] {
        a.fiq_mask(&mut sys, &mut shadow, fiq_hwirq(x));
        a.fiq_unmask(&mut sys, &mut shadow, fiq_hwirq(x));
        a.fiq_eoi(&mut sys, &shadow, fiq_hwirq(x));
    }
    assert!(sys.log.is_empty());
}

// ---- IPIs ----

#[test]
fn fast_ipis_go_local_within_the_cluster_and_global_across() {
    let a = aic(AIC3_INFO, V3_CAPS, true);
    let sim = SimAic::new(AIC3_INFO, V3_CAPS);
    let mut core = sim.core();
    let me = 0x0000_0102u64; // cluster 1, cpu 2
    let mut sys = SimSys::with(&[(Sysreg::Mpidr, me)]);
    a.ipi_send_single(&mut core, &mut sys, 5, 0x0000_0103);
    a.ipi_send_single(&mut core, &mut sys, 1, 0x0000_0001);
    assert_eq!(
        sys.log,
        vec![
            SysOp::Read(Sysreg::Mpidr),
            SysOp::Write(Sysreg::IpiRrLocal, 3),
            SysOp::Isb,
            SysOp::Read(Sysreg::Mpidr),
            SysOp::Write(Sysreg::IpiRrGlobal, 1),
            SysOp::Isb,
        ]
    );
    // Without local fast IPIs (t8015) every IPI is global, with its cluster.
    let b = aic(AIC1_FIPI_INFO, M1_CAPS, true);
    let mut sys = SimSys::with(&[(Sysreg::Mpidr, me)]);
    b.ipi_send_single(&mut core, &mut sys, 5, 0x0000_0103);
    assert_eq!(sys.log[1], SysOp::Write(Sysreg::IpiRrGlobal, 3 | (1 << 16)));
    assert!(sim.writes().is_empty());
}

#[test]
fn thirty_two_virtual_ipis_ride_one_hardware_ipi() {
    assert_eq!(AIC_NR_SWIPI, 32);
    let a = aic(AIC3_INFO, V3_CAPS, true);
    let sim = SimAic::new(AIC3_INFO, V3_CAPS);
    let mut core = sim.core();
    // CPU 1 (cluster 0) unmasks all 32 virtual IPIs; CPU 0 sends each of them to it.
    let mut target = IpiMuxCpu::default();
    for n in 0..32 {
        assert!(!target.unmask(n));
    }
    let mut sender = SimSys::with(&[(Sysreg::Mpidr, 0)]);
    let mut parent_sends = 0;
    for n in (0..32).rev() {
        if target.send(n) {
            a.ipi_send_single(&mut core, &mut sender, 1, 0x1);
            parent_sends += 1;
        }
    }
    // Each newly pending, enabled bit raises the parent IPI (ipi-mux.c:86-87); a repeat does not.
    assert_eq!(parent_sends, 32);
    assert!(!target.send(7));
    assert_eq!(
        sender
            .log
            .iter()
            .filter(|op| **op == SysOp::Write(Sysreg::IpiRrLocal, 1))
            .count(),
        32
    );
    // CPU 1 takes the FIQ: IPI_SR pending, acknowledged, then all 32 delivered lowest first, once.
    let mut sys = SimSys::with(&[(Sysreg::IpiSr, IPI_SR_PENDING)]);
    let mut rec = Rec {
        mux: target,
        ..Rec::default()
    };
    a.handle_fiq(&mut core, &mut sys, &mut rec);
    assert_eq!(rec.delivered, (0..32).collect::<Vec<_>>());
    assert_eq!(rec.mux.bits, 0);
    assert_eq!(sys.get(Sysreg::IpiSr), 0);
}

#[test]
fn a_masked_virtual_ipi_waits_and_is_raised_on_unmask() {
    let mut cpu = IpiMuxCpu::default();
    assert!(!cpu.unmask(3));
    cpu.mask(3);
    assert!(!cpu.send(3));
    assert_eq!(cpu.process(), 0);
    assert!(cpu.unmask(3));
    assert_eq!(cpu.process(), 1 << 3);
    assert_eq!(cpu.bits, 0);
    let mut seen = Vec::new();
    for_each_ipi(0x8000_0011, |n| seen.push(n));
    assert_eq!(seen, vec![0, 4, 31]);
}

// ---- per-CPU init and bulk init ----

#[test]
fn init_cpu_masks_every_fiq_source_in_order() {
    let a = aic(AIC3_INFO, V3_CAPS, true);
    let sim = SimAic::new(AIC3_INFO, V3_CAPS);
    let mut core = sim.core();
    let mut sys = SimSys::with(&[
        (Sysreg::IpiSr, IPI_SR_PENDING),
        (Sysreg::CntpCtlEl0, FIRING),
        (Sysreg::VmTmrFiqEna, 3),
        (Sysreg::IchHcrEl2, ICH_HCR_EL2_EN),
        (Sysreg::Pmcr0, (PMCR0_IMODE_FIQ << 8) | PMCR0_IACT | 0x3),
        (Sysreg::Upmcr0, (UPMCR0_IMODE_FIQ << 16) | 0x5),
    ]);
    let shadow = a.init_cpu(&mut core, &mut sys, &mut Rec::default(), 0);
    assert_eq!(shadow, FiqShadow(0));
    assert_eq!(
        sys.log,
        vec![
            SysOp::Write(Sysreg::IpiSr, IPI_SR_PENDING),
            SysOp::Read(Sysreg::CntpCtlEl0),
            SysOp::Write(Sysreg::CntpCtlEl0, FIRING | ARCH_TIMER_CTRL_IT_MASK),
            SysOp::Read(Sysreg::CntvCtlEl0),
            SysOp::Write(Sysreg::CntvCtlEl0, ARCH_TIMER_CTRL_IT_MASK),
            SysOp::Read(Sysreg::VmTmrFiqEna),
            SysOp::Write(Sysreg::VmTmrFiqEna, 0),
            SysOp::Read(Sysreg::IchHcrEl2),
            SysOp::Write(Sysreg::IchHcrEl2, 0),
            SysOp::Read(Sysreg::Pmcr0),
            SysOp::Write(Sysreg::Pmcr0, 0x3),
            SysOp::Read(Sysreg::Upmcr0),
            SysOp::Write(Sysreg::Upmcr0, 0x5),
            SysOp::Isb,
        ]
    );
    // v2/v3 touch no MMIO here.
    assert!(sim.writes().is_empty());
}

#[test]
fn init_cpu_on_v1_sets_the_hardware_ipi_masks() {
    for (info, expect) in [
        (
            AIC1_INFO,
            vec![
                (AIC_IPI_ACK as usize, AIC_IPI_SELF | AIC_IPI_OTHER),
                (AIC_IPI_MASK_SET as usize, AIC_IPI_SELF),
                (AIC_IPI_MASK_CLR as usize, AIC_IPI_OTHER),
            ],
        ),
        (
            AIC1_LOCAL_FIPI_INFO,
            vec![
                (AIC_IPI_ACK as usize, AIC_IPI_SELF | AIC_IPI_OTHER),
                (AIC_IPI_MASK_SET as usize, AIC_IPI_SELF | AIC_IPI_OTHER),
            ],
        ),
    ] {
        let a = aic(info, M1_CAPS, false);
        let sim = SimAic::new(info, M1_CAPS);
        sim.st.borrow_mut().whoami = 2;
        let mut rec = Rec::default();
        a.init_cpu(&mut sim.core(), &mut SimSys::default(), &mut rec, 2);
        assert!(rec.events.is_empty());
        assert_eq!(sim.writes(), expect);
        let mut rec = Rec::default();
        a.init_cpu(&mut sim.core(), &mut SimSys::default(), &mut rec, 3);
        assert_eq!(
            rec.events,
            vec![Ev::Report(Report::WhoamiMismatch { whoami: 2, cpu: 3 })]
        );
    }
}

#[test]
fn init_hw_masks_clears_and_enables() {
    let a = aic(AIC2_INFO, V2_CAPS, true);
    let sim = SimAic::new(AIC2_INFO, V2_CAPS);
    sim.st
        .borrow_mut()
        .regs
        .insert(AIC2_CONFIG as usize, 0x1000_0000);
    let mut core = sim.core();
    a.init_hw(&mut core);
    let w = sim.writes();
    let words = 600u32.div_ceil(32); // BITS_TO_U32(nr_irq) = 19
    let mut expect = Vec::new();
    for die in 0..2u32 {
        let off = die * a.geo.die_stride;
        for i in 0..words {
            expect.push(((a.geo.mask_set + off + 4 * i) as usize, u32::MAX));
        }
        for i in 0..words {
            expect.push(((a.geo.sw_clr + off + 4 * i) as usize, u32::MAX));
        }
    }
    expect.push((AIC2_CONFIG as usize, 0x1000_0001));
    assert_eq!(w, expect);
    assert!(sim.masked(1, 599) && sim.masked(0, 0));
}

#[test]
fn init_hw_on_v1_routes_every_irq_to_cpu0() {
    let a = aic(AIC1_INFO, M1_CAPS, true);
    let sim = SimAic::new(AIC1_INFO, M1_CAPS);
    a.init_hw(&mut sim.core());
    let w = sim.writes();
    assert_eq!(w.len(), 28 + 28 + 896);
    assert!(
        w[56..]
            .iter()
            .enumerate()
            .all(|(i, &(off, v))| off == 0x3000 + 4 * i && v == 1)
    );
}

#[test]
fn imp_def_encodings_are_the_c_ones() {
    assert_eq!(
        Sysreg::IpiRrLocal.imp_def_encoding(),
        Some((3, 5, 15, 0, 0))
    );
    assert_eq!(Sysreg::IpiSr.imp_def_encoding(), Some((3, 5, 15, 1, 1)));
    assert_eq!(Sysreg::Pmcr0.imp_def_encoding(), Some((3, 1, 15, 0, 0)));
    assert_eq!(Sysreg::CntpCtlEl0.imp_def_encoding(), None);
    assert_eq!(genmask(27, 24), 0x0f00_0000);
    assert_eq!(mask_reg(65), 8);
    assert_eq!(mask_bit(65), 2);
    assert_eq!(aic_cpu_ipi_mask_clr(2), 0x5128);
}
