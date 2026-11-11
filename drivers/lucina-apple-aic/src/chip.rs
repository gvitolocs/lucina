// SPDX-License-Identifier: GPL-2.0-or-later
// Derived from drivers/irqchip/irq-apple-aic.c of AsahiLinux/linux (branch asahi, rev 6ba0637d1ed9):
// Copyright The Asahi Linux Contributors; based on irq-lpc32xx (Copyright 2015-2016 Vladimir Zapolskiy) and
// irq-bcm2836 (Copyright 2015 Broadcom). Behaviour re-expressed in Rust for LucinaOS; see the crate README.
// This is a Rust port of the behaviour of drivers/irqchip/irq-apple-aic.c; the original is the source of truth.

//! The driver's operations over the register traits: the IRQ chip (mask, unmask, eoi, affinity), the FIQ chip and its per-CPU
//! mask shadow, the IRQ and FIQ entry points, IPIs, the per-CPU setup and the probe's bulk initialisation.

use crate::fwspec::{TranslateError, Translated, translate};
use crate::hwirq::{fiq_hwirq, hwirq_die, hwirq_irq, hwirq_type};
use crate::info::{AicInfo, Geometry, Version};
use crate::mmio::{AicHandler, AicMmio, AicSysregs, Report, Sysreg, clear_set};
use crate::regs::*;

/// One probed AIC: what was matched, what was derived, and the exception level the kernel runs at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Aic {
    pub info: AicInfo,
    pub geo: Geometry,
    /// `is_kernel_in_hyp_mode()` (irq-apple-aic.c:428, :586, :722, :872, :1092, :1102).
    pub el2: bool,
}

/// Why `set_affinity` did nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AffinityError {
    /// No `TARGET_CPU` registers: v2/v3, whose `aic2_chip` has no `irq_set_affinity` (irq-apple-aic.c:483-489); the v1 op
    /// itself `BUG_ON`s here (:452).
    NotSupported,
    /// The mask (with `online`, unless forced) names no CPU. The C would compute `BIT(nr_cpu_ids)` here (:454-459).
    NoCpu,
}

/// The per-CPU `aic_fiq_unmasked` shadow (irq-apple-aic.c:349): which FIQ indices the core has unmasked, so that the eoi
/// knows whether to lift the mask the ack put on. Zeroed by [`Aic::init_cpu`] (:916-917).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FiqShadow(pub u32);

impl Aic {
    /// Byte offset and bit of `hwirq` in the bit array starting at `base` (one of the `geo` offsets): `base + die *
    /// die_stride + MASK_REG(irq)` and `MASK_BIT(irq)` (irq-apple-aic.c:374-377), in u32 as the C.
    pub fn bit_reg(&self, base: u32, hwirq: u32) -> (usize, u32) {
        let off = hwirq_die(hwirq).wrapping_mul(self.geo.die_stride);
        let irq = hwirq_irq(hwirq);
        (
            base.wrapping_add(off).wrapping_add(mask_reg(irq)) as usize,
            mask_bit(irq),
        )
    }

    /// `aic_irq_domain_translate` with this AIC's counts and exception level ([`translate`]).
    pub fn translate(&self, cells: &[u32]) -> Result<Translated, TranslateError> {
        translate(cells, self.geo.nr_die, self.geo.nr_irq, self.el2)
    }

    // ---- IRQ chip ----

    /// `aic_irq_mask` (irq-apple-aic.c:369-378): write the IRQ's bit to its die's MASK_SET.
    pub fn irq_mask<C: AicMmio>(&self, core: &mut C, hwirq: u32) {
        let (off, bit) = self.bit_reg(self.geo.mask_set, hwirq);
        core.write32(off, bit);
    }

    /// `aic_irq_unmask` (irq-apple-aic.c:380-389): write the IRQ's bit to its die's MASK_CLR.
    pub fn irq_unmask<C: AicMmio>(&self, core: &mut C, hwirq: u32) {
        let (off, bit) = self.bit_reg(self.geo.mask_clr, hwirq);
        core.write32(off, bit);
    }

    /// `aic_irq_eoi` (irq-apple-aic.c:391-399): the event read masked the IRQ, so the eoi unmasks it unless the core has it
    /// masked (`irqd_irq_masked`, given as `masked`).
    pub fn irq_eoi<C: AicMmio>(&self, core: &mut C, hwirq: u32, masked: bool) {
        if !masked {
            self.irq_unmask(core, hwirq);
        }
    }

    /// `aic_irq_set_affinity` (irq-apple-aic.c:445-463), v1 only. `mask` and `online` are CPU bit masks. Forced: the first CPU
    /// of `mask`; otherwise the first of `mask & online` (`cpumask_any_and` is `cpumask_first_and`, include/linux/cpumask.h:954).
    /// Writes `BIT(cpu)` to `TARGET_CPU + irq * 4` (the die is not used) and returns the CPU, the new effective affinity.
    pub fn irq_set_affinity<C: AicMmio>(
        &self,
        core: &mut C,
        hwirq: u32,
        mask: u64,
        online: u64,
        force: bool,
    ) -> Result<u32, AffinityError> {
        if self.info.target_cpu == 0 {
            return Err(AffinityError::NotSupported);
        }
        let pick = if force { mask } else { mask & online };
        if pick == 0 {
            return Err(AffinityError::NoCpu);
        }
        let cpu = pick.trailing_zeros();
        // irq-apple-aic.c:459: BIT(cpu) is an unsigned long written with writel, so it keeps its low 32 bits.
        let off = self
            .info
            .target_cpu
            .wrapping_add(hwirq_irq(hwirq).wrapping_mul(4));
        core.write32(off as usize, (1u64 << cpu) as u32);
        Ok(cpu)
    }

    // ---- FIQ chip ----

    /// `aic_fiq_set_mask` (irq-apple-aic.c:500-515): "Only the guest timers have real mask bits": clear the timer's enable in
    /// `VM_TMR_FIQ_ENA_EL2`, then `isb`. Every other FIQ has nothing to write. Also the FIQ chip's `irq_ack` (:624).
    pub fn fiq_set_mask<S: AicSysregs>(&self, sys: &mut S, hwirq: u32) {
        match hwirq_irq(hwirq) {
            FIQ_TMR_EL02_PHYS => {
                clear_set(sys, Sysreg::VmTmrFiqEna, VM_TMR_FIQ_ENABLE_P, 0);
                sys.isb();
            }
            FIQ_TMR_EL02_VIRT => {
                clear_set(sys, Sysreg::VmTmrFiqEna, VM_TMR_FIQ_ENABLE_V, 0);
                sys.isb();
            }
            _ => {}
        }
    }

    /// `aic_fiq_clear_mask` (irq-apple-aic.c:517-531): set the guest timer's enable, then `isb`.
    pub fn fiq_clear_mask<S: AicSysregs>(&self, sys: &mut S, hwirq: u32) {
        match hwirq_irq(hwirq) {
            FIQ_TMR_EL02_PHYS => {
                clear_set(sys, Sysreg::VmTmrFiqEna, 0, VM_TMR_FIQ_ENABLE_P);
                sys.isb();
            }
            FIQ_TMR_EL02_VIRT => {
                clear_set(sys, Sysreg::VmTmrFiqEna, 0, VM_TMR_FIQ_ENABLE_V);
                sys.isb();
            }
            _ => {}
        }
    }

    /// `aic_fiq_mask` (irq-apple-aic.c:533-537).
    pub fn fiq_mask<S: AicSysregs>(&self, sys: &mut S, shadow: &mut FiqShadow, hwirq: u32) {
        self.fiq_set_mask(sys, hwirq);
        shadow.0 &= !fiq_bit(hwirq);
    }

    /// `aic_fiq_unmask` (irq-apple-aic.c:539-543).
    pub fn fiq_unmask<S: AicSysregs>(&self, sys: &mut S, shadow: &mut FiqShadow, hwirq: u32) {
        self.fiq_clear_mask(sys, hwirq);
        shadow.0 |= fiq_bit(hwirq);
    }

    /// `aic_fiq_eoi` (irq-apple-aic.c:545-550): "We mask to ack (where we can), so we need to unmask at EOI", if the core has
    /// it unmasked.
    pub fn fiq_eoi<S: AicSysregs>(&self, sys: &mut S, shadow: &FiqShadow, hwirq: u32) {
        if shadow.0 & fiq_bit(hwirq) != 0 {
            self.fiq_clear_mask(sys, hwirq);
        }
    }

    // ---- Entry points ----

    /// `aic_handle_irq` (irq-apple-aic.c:401-443).
    ///
    /// Drains the event register until it reads 0 (:406-421): an IRQ event is handed to the core with the event word itself as
    /// the hwirq (:415-416), the "other" IPI goes to [`Aic::handle_ipi`] (:417-418), anything else non-zero is reported
    /// (:419-420). Each event read acknowledges and masks what it returns (:17, :23, :394-395), so a line that stays asserted
    /// is not returned again until it is unmasked: the loop ends. Then, at EL2 with the virtual GIC enabled and a maintenance
    /// condition pending, the vGIC maintenance FIQ is handled; if that left it enabled and pending, it is reported and the
    /// virtual GIC disabled (:428-442).
    pub fn handle_irq<C: AicMmio, E: AicMmio, S: AicSysregs, H: AicHandler>(
        &self,
        core: &mut C,
        event: &E,
        sys: &mut S,
        h: &mut H,
    ) {
        loop {
            let ev = event.read32(self.info.event as usize);
            let ty = hwirq_type(ev);
            let num = hwirq_irq(ev);
            if ty == AIC_EVENT_TYPE_IRQ {
                h.handle_hwirq(ev);
            } else if ty == AIC_EVENT_TYPE_IPI && num == AIC_EVENT_IPI_OTHER {
                self.handle_ipi(core, sys, h);
            } else if ev != 0 {
                h.report(Report::UnknownEvent { ty, num });
            }
            if ev == 0 {
                break;
            }
        }
        if self.el2
            && sys.read(Sysreg::IchHcrEl2) & ICH_HCR_EL2_EN != 0
            && sys.read(Sysreg::IchMisrEl2) != 0
        {
            h.handle_hwirq(fiq_hwirq(FIQ_VGIC_MI));
            if sys.read(Sysreg::IchHcrEl2) & ICH_HCR_EL2_EN != 0 {
                let misr = sys.read(Sysreg::IchMisrEl2);
                if misr != 0 {
                    h.report(Report::VgicNotHandled { misr });
                    clear_set(sys, Sysreg::IchHcrEl2, ICH_HCR_EL2_EN, 0);
                }
            }
        }
    }

    /// `aic_handle_fiq` (irq-apple-aic.c:557-613). There is no FIQ source register, so every source is polled, in this order:
    ///
    /// 1. with fast IPIs, `IPI_SR` pending: [`Aic::handle_ipi`] (:574-576);
    /// 2. `cntp_ctl_el0` firing: FIQ `AIC_TMR_EL0_PHYS` (:578-580);
    /// 3. `cntv_ctl_el0` firing: FIQ `AIC_TMR_EL0_VIRT` (:582-584);
    /// 4. at EL2, with `VM_TMR_FIQ_ENA` read once: P enabled and `CNTP_CTL_EL02` firing: FIQ `AIC_TMR_EL02_PHYS`; V enabled
    ///    and `CNTV_CTL_EL02` firing: FIQ `AIC_TMR_EL02_VIRT` (:586-598); a timer register is read only if its enable is set;
    /// 5. `PMCR0` in FIQ mode with `IACT`: FIQ `AIC_CPU_PMU_P` (:600-603), the merged PMU interrupt;
    /// 6. with fast IPIs, `UPMCR0` in FIQ mode and `UPMSR.IACT`: reported, and `UPMCR0`'s mode set to off (:605-612);
    ///    `UPMSR` is read only in FIQ mode.
    pub fn handle_fiq<C: AicMmio, S: AicSysregs, H: AicHandler>(
        &self,
        core: &mut C,
        sys: &mut S,
        h: &mut H,
    ) {
        if self.info.fast_ipi && sys.read(Sysreg::IpiSr) & IPI_SR_PENDING != 0 {
            self.handle_ipi(core, sys, h);
        }
        if timer_firing(sys.read(Sysreg::CntpCtlEl0)) {
            h.handle_hwirq(fiq_hwirq(FIQ_TMR_EL0_PHYS));
        }
        if timer_firing(sys.read(Sysreg::CntvCtlEl0)) {
            h.handle_hwirq(fiq_hwirq(FIQ_TMR_EL0_VIRT));
        }
        if self.el2 {
            let enabled = sys.read(Sysreg::VmTmrFiqEna);
            if enabled & VM_TMR_FIQ_ENABLE_P != 0 && timer_firing(sys.read(Sysreg::CntpCtlEl02)) {
                h.handle_hwirq(fiq_hwirq(FIQ_TMR_EL02_PHYS));
            }
            if enabled & VM_TMR_FIQ_ENABLE_V != 0 && timer_firing(sys.read(Sysreg::CntvCtlEl02)) {
                h.handle_hwirq(fiq_hwirq(FIQ_TMR_EL02_VIRT));
            }
        }
        if sys.read(Sysreg::Pmcr0) & (PMCR0_IMODE | PMCR0_IACT)
            == ((PMCR0_IMODE_FIQ << 8) | PMCR0_IACT)
        {
            h.handle_hwirq(fiq_hwirq(AIC_CPU_PMU_P));
        }
        if self.info.fast_ipi
            && (sys.read(Sysreg::Upmcr0) & UPMCR0_IMODE) >> 16 == UPMCR0_IMODE_FIQ
            && sys.read(Sysreg::Upmsr) & UPMSR_IACT != 0
        {
            h.report(Report::UncorePmcMasked);
            clear_set(sys, Sysreg::Upmcr0, UPMCR0_IMODE, UPMCR0_IMODE_OFF << 16);
        }
    }

    /// `aic_handle_ipi` (irq-apple-aic.c:812-836): acknowledge (fast: write `IPI_SR_PENDING` to `IPI_SR`, then `isb` so the
    /// ack is ordered before the vIPI handling; v1 without fast IPIs: `AIC_IPI_ACK` = `AIC_IPI_OTHER`), process the virtual
    /// IPIs, and without fast IPIs unmask the hardware IPI again (`AIC_IPI_MASK_CLR` = `AIC_IPI_OTHER`).
    pub fn handle_ipi<C: AicMmio, S: AicSysregs, H: AicHandler>(
        &self,
        core: &mut C,
        sys: &mut S,
        h: &mut H,
    ) {
        if self.info.fast_ipi {
            sys.write(Sysreg::IpiSr, IPI_SR_PENDING);
            sys.isb();
        } else {
            core.write32(AIC_IPI_ACK as usize, AIC_IPI_OTHER);
        }
        h.ipi_mux_process();
        if !self.info.fast_ipi {
            core.write32(AIC_IPI_MASK_CLR as usize, AIC_IPI_OTHER);
        }
    }

    /// `aic_ipi_send_single` (irq-apple-aic.c:838-844), the mux's parent send: with fast IPIs `aic_ipi_send_fast`
    /// (:796-810), else `AIC_IPI_SEND` = `BIT(cpu)`. `target_mpidr` is `cpu_logical_map(cpu)`.
    ///
    /// Fast: if local fast IPIs exist and the target is in this CPU's cluster, write its CPU field to `IPI_RR_LOCAL`, else
    /// write CPU and cluster to `IPI_RR_GLOBAL`; the type field stays 0 (`IPI_RR_IMMEDIATE`); then `isb`.
    pub fn ipi_send_single<C: AicMmio, S: AicSysregs>(
        &self,
        core: &mut C,
        sys: &mut S,
        cpu: u32,
        target_mpidr: u64,
    ) {
        if self.info.fast_ipi {
            let my_mpidr = sys.read(Sysreg::Mpidr);
            let cluster = mpidr_cluster(target_mpidr);
            let idx = mpidr_cpu(target_mpidr);
            if self.info.local_fast_ipi && mpidr_cluster(my_mpidr) == cluster {
                sys.write(
                    Sysreg::IpiRrLocal,
                    u64::from(field_prep(IPI_RR_CPU, idx as u32)),
                );
            } else {
                sys.write(
                    Sysreg::IpiRrGlobal,
                    u64::from(
                        field_prep(IPI_RR_CPU, idx as u32)
                            | field_prep(IPI_RR_CLUSTER, cluster as u32),
                    ),
                );
            }
            sys.isb();
        } else {
            // AIC_IPI_SEND_CPU(cpu) = BIT(cpu) (irq-apple-aic.c:93).
            core.write32(AIC_IPI_SEND as usize, (1u64 << cpu) as u32);
        }
    }

    /// `aic_init_cpu` (irq-apple-aic.c:859-920), run on each CPU as it starts: masks every per-CPU FIQ source, on v1 sets
    /// the hardware IPI masks, and returns the zeroed FIQ shadow.
    ///
    /// Order: fast IPIs: ack `IPI_SR`; set `IT_MASK` in both EL0 timers; at EL2 clear both guest-timer FIQ enables and
    /// `ICH_HCR_EL2.En`; `PMCR0` mode off and `IACT` cleared; fast IPIs: `UPMCR0` mode off; `isb`. Then on v1: check
    /// `AIC_WHOAMI` against `cpu`, ack both IPIs, and mask both (fast IPIs) or mask "self" and unmask "other".
    pub fn init_cpu<C: AicMmio, S: AicSysregs, H: AicHandler>(
        &self,
        core: &mut C,
        sys: &mut S,
        h: &mut H,
        cpu: u32,
    ) -> FiqShadow {
        if self.info.fast_ipi {
            sys.write(Sysreg::IpiSr, IPI_SR_PENDING); // :864-865
        }
        clear_set(sys, Sysreg::CntpCtlEl0, 0, ARCH_TIMER_CTRL_IT_MASK); // :868
        clear_set(sys, Sysreg::CntvCtlEl0, 0, ARCH_TIMER_CTRL_IT_MASK); // :869
        if self.el2 {
            clear_set(
                sys,
                Sysreg::VmTmrFiqEna,
                VM_TMR_FIQ_ENABLE_V | VM_TMR_FIQ_ENABLE_P,
                0,
            ); // :874-875
            clear_set(sys, Sysreg::IchHcrEl2, ICH_HCR_EL2_EN, 0); // :878
        }
        clear_set(
            sys,
            Sysreg::Pmcr0,
            PMCR0_IMODE | PMCR0_IACT,
            PMCR0_IMODE_OFF << 8,
        ); // :882-883
        if self.info.fast_ipi {
            clear_set(sys, Sysreg::Upmcr0, UPMCR0_IMODE, UPMCR0_IMODE_OFF << 16); // :886-889
        }
        sys.isb(); // :892
        if self.info.version == Version::V1 {
            let whoami = core.read32(AIC_WHOAMI as usize); // :900
            if whoami != cpu {
                h.report(Report::WhoamiMismatch { whoami, cpu });
            }
            core.write32(AIC_IPI_ACK as usize, AIC_IPI_SELF | AIC_IPI_OTHER); // :907
            if self.info.fast_ipi {
                core.write32(AIC_IPI_MASK_SET as usize, AIC_IPI_SELF | AIC_IPI_OTHER); // :909
            } else {
                core.write32(AIC_IPI_MASK_SET as usize, AIC_IPI_SELF); // :911
                core.write32(AIC_IPI_MASK_CLR as usize, AIC_IPI_OTHER); // :912
            }
        }
        FiqShadow(0) // :917
    }

    /// The probe's hardware initialisation (irq-apple-aic.c:1073-1090): per die, every IRQ masked (`BITS_TO_U32(nr_irq)`
    /// words of MASK_SET written all-ones), every software trigger cleared (SW_CLR likewise), and on v1 every IRQ routed to
    /// CPU 0 (`TARGET_CPU[i]` = 1); then on v2/v3 `AIC2_CONFIG` read, `ENABLE` set, written back.
    pub fn init_hw<C: AicMmio>(&self, core: &mut C) {
        let words = bits_to_u32(self.geo.nr_irq);
        let mut off = 0u32;
        for _die in 0..self.geo.nr_die {
            for i in 0..words {
                core.write32(
                    self.geo.mask_set.wrapping_add(off).wrapping_add(i * 4) as usize,
                    u32::MAX,
                );
            }
            for i in 0..words {
                core.write32(
                    self.geo.sw_clr.wrapping_add(off).wrapping_add(i * 4) as usize,
                    u32::MAX,
                );
            }
            if self.info.target_cpu != 0 {
                for i in 0..self.geo.nr_irq {
                    core.write32(
                        self.info.target_cpu.wrapping_add(off).wrapping_add(i * 4) as usize,
                        1,
                    );
                }
            }
            off = off.wrapping_add(self.geo.die_stride);
        }
        if matches!(self.info.version, Version::V2 | Version::V3) {
            let config = core.read32(AIC2_CONFIG as usize) | AIC2_CONFIG_ENABLE;
            core.write32(AIC2_CONFIG as usize, config);
        }
    }
}

/// `BIT(aic_fiq_get_idx(d))` (irq-apple-aic.c:495-498, :536, :542, :548). The index is below `AIC_NR_FIQ` for every FIQ
/// [`translate`] produces; a larger one gives no bit here, where the C shift would be out of range of the u32 shadow.
fn fiq_bit(hwirq: u32) -> u32 {
    1u32.checked_shl(hwirq_irq(hwirq)).unwrap_or(0)
}
