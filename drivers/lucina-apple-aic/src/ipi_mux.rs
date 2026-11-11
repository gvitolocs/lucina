// SPDX-License-Identifier: GPL-2.0-or-later
// Derived from kernel/irq/ipi-mux.c of AsahiLinux/linux (branch asahi, rev 6ba0637d1ed9): Copyright The Asahi Linux
// Contributors, Copyright (c) 2022 Ventana Micro Systems Inc. Behaviour re-expressed in Rust for LucinaOS; see the crate README.
// This is a Rust port of the behaviour of kernel/irq/ipi-mux.c; the original is the source of truth.

//! The virtual IPIs the AIC driver multiplexes onto one hardware IPI per CPU: `aic_init_smp` creates `AIC_NR_SWIPI` = 32 of
//! them with `ipi_mux_create(AIC_NR_SWIPI, aic_ipi_send_single)` and gives them to the SMP code as IPIs `0..32`
//! (`set_smp_ipi_range(base_ipi, AIC_NR_SWIPI)`, irq-apple-aic.c:846-857). Virtual IPI `n` is bit `n` of the per-CPU words
//! below (its hwirq in the mux domain, ipi-mux.c:105).
//!
//! The C keeps `enable` and `bits` as atomics with the barriers its comments explain (ipi-mux.c:20-23, :43-47, :64-85,
//! :127-138). This model is the sequential behaviour of each step; the kernel side keeps the atomics and their ordering.

use crate::regs::AIC_NR_SWIPI;

/// `struct ipi_mux_cpu` (ipi-mux.c:20-23): one per CPU.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IpiMuxCpu {
    /// Unmasked virtual IPIs.
    pub enable: u32,
    /// Sent and not yet processed.
    pub bits: u32,
}

impl IpiMuxCpu {
    /// `ipi_mux_mask` (ipi-mux.c:29-34).
    pub fn mask(&mut self, n: u32) {
        assert!(n < AIC_NR_SWIPI);
        self.enable &= !(1 << n);
    }

    /// `ipi_mux_unmask` (ipi-mux.c:36-52). True if a parent IPI must be sent to this CPU now: "If a pending IPI was
    /// unmasked, raise a parent IPI immediately" (:49-51).
    #[must_use]
    pub fn unmask(&mut self, n: u32) -> bool {
        assert!(n < AIC_NR_SWIPI);
        self.enable |= 1 << n;
        self.bits & (1 << n) != 0
    }

    /// `ipi_mux_send_mask` for one target CPU (ipi-mux.c:61-88): marks `n` pending on the target; true if a parent IPI must
    /// be sent to it (`aic_ipi_send_single(cpu)`), which is when `n` was not pending already and is enabled there (:86-87).
    #[must_use]
    pub fn send(&mut self, n: u32) -> bool {
        assert!(n < AIC_NR_SWIPI);
        let pending = self.bits;
        self.bits |= 1 << n;
        pending & (1 << n) == 0 && self.enable & (1 << n) != 0
    }

    /// `ipi_mux_process` (ipi-mux.c:120-142): takes the pending enabled virtual IPIs off this CPU and returns them; the C
    /// then handles them lowest bit first (`for_each_set_bit`, :140-141), see [`for_each_ipi`].
    #[must_use]
    pub fn process(&mut self) -> u32 {
        let en = self.enable;
        let ipis = self.bits & en;
        self.bits &= !en;
        ipis
    }
}

/// The virtual IPIs of `ipis`, lowest first, as `for_each_set_bit` walks them (ipi-mux.c:140).
pub fn for_each_ipi(ipis: u32, mut f: impl FnMut(u32)) {
    let mut rest = ipis;
    while rest != 0 {
        let n = rest.trailing_zeros();
        f(n);
        rest &= rest - 1;
    }
}
