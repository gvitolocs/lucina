// SPDX-License-Identifier: GPL-2.0-or-later
// Derived from drivers/irqchip/irq-apple-aic.c of AsahiLinux/linux (branch asahi, rev 6ba0637d1ed9):
// Copyright The Asahi Linux Contributors; based on irq-lpc32xx (Copyright 2015-2016 Vladimir Zapolskiy) and
// irq-bcm2836 (Copyright 2015 Broadcom). Behaviour re-expressed in Rust for LucinaOS; see the crate README.
// This is a Rust port of the behaviour of drivers/irqchip/irq-apple-aic.c; the original is the source of truth.

//! The three things the driver logic talks to: the AIC's MMIO windows, the CPU's system registers, and the kernel's interrupt
//! core. The kernel implements them over real hardware; the tests implement them over a software register file.

/// One MMIO window of the AIC: the core window (DT `reg` 0, `irqc->base`, irq-apple-aic.c:975) or, on v2/v3, the event
/// window (DT `reg` 1, `irqc->event`, :1025). Offsets are bytes from the window's start.
///
/// The C makes every access relaxed (`readl_relaxed`/`writel_relaxed`, irq-apple-aic.c:355-363) except the event read in
/// `aic_handle_irq`, which is a full `readl` so that DMA reads after it are ordered after the interrupt (:407-411). The
/// window passed as the event window must therefore make `read32` an ordered read (a read followed by a barrier, as
/// `readl`); the core window may be relaxed. On v1 the event register sits in the core window (:1007), so the kernel passes
/// a second, ordered handle to the same mapping.
pub trait AicMmio {
    fn read32(&self, off: usize) -> u32;
    fn write32(&mut self, off: usize, v: u32);
}

/// The system registers the driver reads and writes (irq-apple-aic.c:166-208 for the IMP-DEF ones).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Sysreg {
    /// `SYS_IMP_APL_IPI_RR_LOCAL_EL1` (irq-apple-aic.c:171): fast IPI to a CPU of this cluster.
    IpiRrLocal,
    /// `SYS_IMP_APL_IPI_RR_GLOBAL_EL1` (irq-apple-aic.c:172): fast IPI to any CPU.
    IpiRrGlobal,
    /// `SYS_IMP_APL_IPI_SR_EL1` (irq-apple-aic.c:183): fast IPI pending status; the C writes `IPI_SR_PENDING` to it to
    /// acknowledge (:822, :865).
    IpiSr,
    /// `SYS_IMP_APL_VM_TMR_FIQ_ENA_EL2` (irq-apple-aic.c:187): FIQ enables of the two guest timers.
    VmTmrFiqEna,
    /// `SYS_IMP_APL_UPMCR0_EL1` (irq-apple-aic.c:195): uncore PMC control.
    Upmcr0,
    /// `SYS_IMP_APL_UPMSR_EL1` (irq-apple-aic.c:203): uncore PMC status.
    Upmsr,
    /// `SYS_IMP_APL_PMCR0_EL1` (asm/apple_m1_pmu.h:22, used at irq-apple-aic.c:600 and :882): per-core PMC control.
    Pmcr0,
    /// `cntp_ctl_el0` (irq-apple-aic.c:578, :868): architectural.
    CntpCtlEl0,
    /// `cntv_ctl_el0` (irq-apple-aic.c:582, :869): architectural.
    CntvCtlEl0,
    /// `SYS_CNTP_CTL_EL02` (irq-apple-aic.c:590): architectural, VHE only.
    CntpCtlEl02,
    /// `SYS_CNTV_CTL_EL02` (irq-apple-aic.c:595): architectural, VHE only.
    CntvCtlEl02,
    /// `SYS_ICH_HCR_EL2` (irq-apple-aic.c:429, :878): architectural (GICv3 virtualisation).
    IchHcrEl2,
    /// `SYS_ICH_MISR_EL2` (irq-apple-aic.c:430): architectural.
    IchMisrEl2,
    /// `MPIDR_EL1`, read by `read_cpuid_mpidr()` (irq-apple-aic.c:799): architectural.
    Mpidr,
}

impl Sysreg {
    /// The `sys_reg(op0, op1, CRn, CRm, op2)` encoding of the implementation-defined registers, for `mrs`/`msr` with
    /// `S<op0>_<op1>_C<n>_C<m>_<op2>`. `None` for the architectural ones, which the kernel reaches by their names.
    pub const fn imp_def_encoding(self) -> Option<(u8, u8, u8, u8, u8)> {
        match self {
            Sysreg::IpiRrLocal => Some((3, 5, 15, 0, 0)), // irq-apple-aic.c:171
            Sysreg::IpiRrGlobal => Some((3, 5, 15, 0, 1)), // irq-apple-aic.c:172
            Sysreg::IpiSr => Some((3, 5, 15, 1, 1)),      // irq-apple-aic.c:183
            Sysreg::VmTmrFiqEna => Some((3, 5, 15, 1, 3)), // irq-apple-aic.c:187
            Sysreg::Upmcr0 => Some((3, 7, 15, 0, 4)),     // irq-apple-aic.c:195
            Sysreg::Upmsr => Some((3, 7, 15, 6, 4)),      // irq-apple-aic.c:203
            Sysreg::Pmcr0 => Some((3, 1, 15, 0, 0)),      // asm/apple_m1_pmu.h:22
            _ => None,
        }
    }
}

/// `SYS_IMP_APL_IPI_CR_EL1` = sys_reg(3, 5, 15, 3, 1), "Deferred IPI countdown register" (irq-apple-aic.c:191-192).
/// Declared, never used by the C.
pub const IPI_CR_ENCODING: (u8, u8, u8, u8, u8) = (3, 5, 15, 3, 1);

/// The CPU's system registers, as the running CPU sees them.
pub trait AicSysregs {
    fn read(&mut self, r: Sysreg) -> u64;
    fn write(&mut self, r: Sysreg, v: u64);
    /// `isb()`. The C places one after each guest-timer enable change (irq-apple-aic.c:506, :510, :522, :526), after a fast
    /// IPI request (:809), between the fast IPI acknowledge and the vIPI processing (:823), and after the per-CPU masking
    /// (:892).
    fn isb(&mut self);
}

/// `sysreg_clear_set` / `sysreg_clear_set_s` (arch/arm64/include/asm/sysreg.h:1217-1236): read, clear then set, and write only
/// if the value changed.
pub fn clear_set<S: AicSysregs>(s: &mut S, r: Sysreg, clear: u64, set: u64) {
    let val = s.read(r);
    let new = (val & !clear) | set;
    if new != val {
        s.write(r, new);
    }
}

/// Something the C reports with `pr_err_ratelimited` or `WARN_ON` and carries on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Report {
    /// "Unknown IRQ event %d, %d" (irq-apple-aic.c:419-420): an event that is neither an IRQ nor the "other" IPI.
    UnknownEvent { ty: u32, num: u32 },
    /// "vGIC IRQ fired and not handled by KVM (MISR=%llx), disabling." (irq-apple-aic.c:436-440).
    VgicNotHandled { misr: u64 },
    /// "Uncore PMC FIQ fired. Masking." (irq-apple-aic.c:609).
    UncorePmcMasked,
    /// `WARN_ON(aic_ic_read(aic_irqc, AIC_WHOAMI) != smp_processor_id())` on v1 (irq-apple-aic.c:900).
    WhoamiMismatch { whoami: u32, cpu: u32 },
}

/// The kernel's interrupt core, as the dispatch code calls it.
pub trait AicHandler {
    /// `generic_handle_domain_irq(aic_irqc->hw_domain, hwirq)` (irq-apple-aic.c:416, :433, :579-602).
    fn handle_hwirq(&mut self, hwirq: u32);
    /// `ipi_mux_process()` (irq-apple-aic.c:828): deliver the pending virtual IPIs of this CPU ([`crate::ipi_mux`]).
    fn ipi_mux_process(&mut self);
    /// A `pr_err_ratelimited` or `WARN_ON` of the C.
    fn report(&mut self, r: Report);
}
