// SPDX-License-Identifier: GPL-2.0-or-later
// Derived from drivers/irqchip/irq-apple-aic.c of AsahiLinux/linux (branch asahi, rev 6ba0637d1ed9):
// Copyright The Asahi Linux Contributors; based on irq-lpc32xx (Copyright 2015-2016 Vladimir Zapolskiy) and
// irq-bcm2836 (Copyright 2015 Broadcom). Behaviour re-expressed in Rust for LucinaOS; see the crate README.
// This is a Rust port of the behaviour of drivers/irqchip/irq-apple-aic.c; the original is the source of truth.

//! Device-tree interrupt specifiers to hwirq numbers and back (`aic_irq_domain_translate` and its neighbours,
//! irq-apple-aic.c:629-790).
//!
//! The binding (irq-apple-aic.c:37-43): `<AIC_IRQ nr flags>` is hardware IRQ `nr`, `<AIC_FIQ nr flags>` is FIQ `nr`; a
//! 4-cell form puts a die number after the first cell (:698-701).

use crate::hwirq::{fiq_hwirq, hwirq_die, hwirq_irq, hwirq_type, irq_hwirq};
use crate::info::Version;
use crate::regs::*;

/// Why a specifier is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TranslateError {
    /// `-EINVAL`: wrong cell count, unknown first cell, die or number out of range (irq-apple-aic.c:692-694, :705-715, :743).
    Invalid,
    /// `-ENOENT`: a hypervisor timer asked for while the kernel runs at EL1 (irq-apple-aic.c:730-732).
    NoEntry,
}

/// A translated specifier: the hwirq and the trigger type (`*type`, irq-apple-aic.c:746).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Translated {
    pub hwirq: u32,
    /// The flags cell masked with `IRQ_TYPE_SENSE_MASK`.
    pub sense: u32,
}

/// `aic_irq_domain_translate` (irq-apple-aic.c:683-749) for the cells of one specifier.
///
/// `nr_die` and `nr_irq` are the probe's ([`crate::info::Geometry`]); `el2` is `is_kernel_in_hyp_mode()`. (The C also refuses
/// a specifier whose fwnode is not an OF node, :693; every specifier here comes from the device tree.)
pub fn translate(
    cells: &[u32],
    nr_die: u32,
    nr_irq: u32,
    el2: bool,
) -> Result<Translated, TranslateError> {
    // irq-apple-aic.c:692-694
    if cells.len() < 3 || cells.len() > 4 {
        return Err(TranslateError::Invalid);
    }
    // irq-apple-aic.c:696-701: args start at cell 1; with 4 cells, cell 1 is the die.
    let (die, args) = if cells.len() == 4 {
        (cells[1], &cells[2..])
    } else {
        (0, &cells[1..])
    };
    let hwirq = match cells[0] {
        DT_AIC_IRQ => {
            // irq-apple-aic.c:704-709
            if die >= nr_die || args[0] >= nr_irq {
                return Err(TranslateError::Invalid);
            }
            irq_hwirq(die, args[0])
        }
        DT_AIC_FIQ => {
            // irq-apple-aic.c:711-716
            if die != 0 || args[0] >= AIC_NR_FIQ {
                return Err(TranslateError::Invalid);
            }
            let mut h = fiq_hwirq(args[0]);
            // irq-apple-aic.c:718-736: at EL1 the non-redirected timer registers are the guest's.
            if !el2 {
                match args[0] {
                    AIC_TMR_GUEST_PHYS => h = fiq_hwirq(FIQ_TMR_EL0_PHYS),
                    AIC_TMR_GUEST_VIRT => h = fiq_hwirq(FIQ_TMR_EL0_VIRT),
                    AIC_TMR_HV_PHYS | AIC_TMR_HV_VIRT => return Err(TranslateError::NoEntry),
                    _ => {}
                }
            }
            // irq-apple-aic.c:738-740: "Merge the two PMUs on a single interrupt". The C compares the packed hwirq with
            // the bare index; a FIQ hwirq has type 0 and die 0, so it equals its index.
            if h == AIC_CPU_PMU_E {
                h = AIC_CPU_PMU_P;
            }
            h
        }
        // irq-apple-aic.c:742-743
        _ => return Err(TranslateError::Invalid),
    };
    Ok(Translated {
        hwirq,
        sense: args[1] & IRQ_TYPE_SENSE_MASK,
    })
}

/// What a hwirq is, for display.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decoded {
    /// A hardware IRQ: `AIC_IRQ_HWIRQ(die, num)`.
    Irq { die: u32, num: u32 },
    /// A FIQ: `AIC_FIQ_HWIRQ(num)`, `num` an `enum fiq_hwirq` index (irq-apple-aic.c:228-239).
    Fiq { num: u32 },
    /// Any other shape (not produced by [`translate`]).
    Other { die: u32, ty: u32, num: u32 },
}

/// Splits a hwirq into its fields (irq-apple-aic.c:215-216, type :79).
pub fn decode(hwirq: u32) -> Decoded {
    let (die, ty, num) = (hwirq_die(hwirq), hwirq_type(hwirq), hwirq_irq(hwirq));
    match ty {
        AIC_EVENT_TYPE_IRQ => Decoded::Irq { die, num },
        AIC_EVENT_TYPE_FIQ if die == 0 => Decoded::Fiq { num },
        _ => Decoded::Other { die, ty, num },
    }
}

/// The inverse of [`translate`]: a specifier that translates to `hwirq` with trigger `sense`, as `(cells, count)`.
///
/// A hardware IRQ on die 0 gets the 3-cell form, on another die the 4-cell form. A FIQ gets its binding index back: at EL2
/// the index is the hwirq; at EL1 the EL0 timers are the guest timers (the reverse of irq-apple-aic.c:724-729). `None` for a
/// hwirq [`translate`] never produces: the E-core PMU (merged into the P-core one, :738-740), the EL02 timers at EL1, a FIQ
/// index past `AIC_NR_FIQ`, or another shape.
pub fn to_cells(hwirq: u32, sense: u32, el2: bool) -> Option<([u32; 4], usize)> {
    match decode(hwirq) {
        Decoded::Irq { die: 0, num } => Some(([DT_AIC_IRQ, num, sense, 0], 3)),
        Decoded::Irq { die, num } => Some(([DT_AIC_IRQ, die, num, sense], 4)),
        Decoded::Fiq { num } => {
            if num >= AIC_NR_FIQ || num == AIC_CPU_PMU_E {
                return None;
            }
            let index = if el2 {
                num
            } else {
                match num {
                    FIQ_TMR_EL0_PHYS => AIC_TMR_GUEST_PHYS,
                    FIQ_TMR_EL0_VIRT => AIC_TMR_GUEST_VIRT,
                    FIQ_TMR_EL02_PHYS | FIQ_TMR_EL02_VIRT => return None,
                    n => n,
                }
            };
            Some(([DT_AIC_FIQ, index, sense, 0], 3))
        }
        Decoded::Other { .. } => None,
    }
}

/// The FIQ index whose affinity `aic_irq_get_fwspec_info` looks up for a specifier (irq-apple-aic.c:656-681): `None` for a
/// hardware IRQ (no affinity given, :664-665), else the index cell (:667-670): cell 1 with 3 cells, cell 2 otherwise.
///
/// The index is the binding's, before [`translate`]'s EL1 remap and PMU merge: the E-core PMU specifier gets the E-core
/// affinity (`fiq_aff[AIC_CPU_PMU_E]`) although it translates to the P-core PMU hwirq. The C indexes `fiq_aff[intid]`
/// without a bound check (:672); here an index past `AIC_NR_FIQ`, or a count below 3, gives `None`.
pub fn fiq_affinity_index(cells: &[u32]) -> Option<u32> {
    if cells.len() < 3 || cells[0] != DT_AIC_FIQ {
        return None;
    }
    let intid = if cells.len() == 3 { cells[1] } else { cells[2] };
    (intid < AIC_NR_FIQ).then_some(intid)
}

/// Which irq chip and flow a hwirq gets (`aic_irq_domain_map`, irq-apple-aic.c:633-654).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChipKind {
    /// `aic_chip` with `handle_fasteoi_irq`, single target (v1, :474-481, :643-646): has `set_affinity`.
    Aic,
    /// `aic2_chip` with `handle_fasteoi_irq`, single target (v2/v3, :483-489, :640-641): no `set_affinity`.
    Aic2,
    /// `fiq_chip`, per-CPU (`irq_set_percpu_devid`, `handle_percpu_devid_irq`, :620-627, :647-651): every hwirq whose type is
    /// not IRQ.
    Fiq,
}

/// `aic_irq_domain_map`'s choice for `hwirq` (irq-apple-aic.c:633-654).
pub fn chip_for(version: Version, hwirq: u32) -> ChipKind {
    if hwirq_type(hwirq) == AIC_EVENT_TYPE_IRQ {
        match version {
            Version::V1 => ChipKind::Aic,
            Version::V2 | Version::V3 => ChipKind::Aic2,
        }
    } else {
        ChipKind::Fiq
    }
}

/// `aic_irq_set_type` (irq-apple-aic.c:465-472): a hardware IRQ accepts level-high or rising-edge ("MSIs ... implicitly have
/// edge semantics").
pub fn irq_set_type(sense: u32) -> Result<(), TranslateError> {
    if sense == IRQ_TYPE_LEVEL_HIGH || sense == IRQ_TYPE_EDGE_RISING {
        Ok(())
    } else {
        Err(TranslateError::Invalid)
    }
}

/// `aic_fiq_set_type` (irq-apple-aic.c:615-618): a FIQ accepts level-high only.
pub fn fiq_set_type(sense: u32) -> Result<(), TranslateError> {
    if sense == IRQ_TYPE_LEVEL_HIGH {
        Ok(())
    } else {
        Err(TranslateError::Invalid)
    }
}
