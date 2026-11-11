// SPDX-License-Identifier: GPL-2.0-or-later
// Derived from drivers/irqchip/irq-apple-aic.c of AsahiLinux/linux (branch asahi, rev 6ba0637d1ed9):
// Copyright The Asahi Linux Contributors; based on irq-lpc32xx (Copyright 2015-2016 Vladimir Zapolskiy) and
// irq-bcm2836 (Copyright 2015 Broadcom). Behaviour re-expressed in Rust for LucinaOS; see the crate README.
// This is a Rust port of the behaviour of drivers/irqchip/irq-apple-aic.c; the original is the source of truth.

//! Every register offset, field and constant the driver uses, each with the line of the pinned C file (or of the header it
//! includes) it comes from. Constants the C declares but never uses are kept too, marked so, so that nothing has to be looked
//! up again; their meaning beyond the name is not known from the C.

/// `GENMASK(h, l)` for a 32-bit register.
pub const fn genmask(h: u32, l: u32) -> u32 {
    (u32::MAX >> (31 - h)) & (u32::MAX << l)
}

/// `FIELD_GET(mask, v)`.
pub const fn field_get(mask: u32, v: u32) -> u32 {
    (v & mask) >> mask.trailing_zeros()
}

/// `FIELD_PREP(mask, v)`: shifted into place and masked to the field.
pub const fn field_prep(mask: u32, v: u32) -> u32 {
    (v << mask.trailing_zeros()) & mask
}

// ---- AIC v1 (MMIO) ----

/// `AIC_INFO` (irq-apple-aic.c:71), read at :999.
pub const AIC_INFO: u32 = 0x0004;
/// `AIC_INFO_NR_IRQ` = GENMASK(15, 0) (irq-apple-aic.c:72).
pub const AIC_INFO_NR_IRQ: u32 = genmask(15, 0);
/// `AIC_CONFIG` (irq-apple-aic.c:74). Declared, never used by the C.
pub const AIC_CONFIG: u32 = 0x0010;
/// `AIC_WHOAMI` (irq-apple-aic.c:76), read at :900.
pub const AIC_WHOAMI: u32 = 0x2000;
/// `AIC_EVENT` (irq-apple-aic.c:77): the v1 event register, in the core window (`irqc->event = irqc->base`, :1007).
pub const AIC_EVENT: u32 = 0x2004;
/// `AIC_EVENT_DIE` = GENMASK(31, 24) (irq-apple-aic.c:78).
pub const AIC_EVENT_DIE: u32 = genmask(31, 24);
/// `AIC_EVENT_TYPE` = GENMASK(23, 16) (irq-apple-aic.c:79).
pub const AIC_EVENT_TYPE: u32 = genmask(23, 16);
/// `AIC_EVENT_NUM` = GENMASK(15, 0) (irq-apple-aic.c:80).
pub const AIC_EVENT_NUM: u32 = genmask(15, 0);
/// `AIC_EVENT_TYPE_FIQ` = 0, "Software use" (irq-apple-aic.c:82).
pub const AIC_EVENT_TYPE_FIQ: u32 = 0;
/// `AIC_EVENT_TYPE_IRQ` = 1 (irq-apple-aic.c:83).
pub const AIC_EVENT_TYPE_IRQ: u32 = 1;
/// `AIC_EVENT_TYPE_IPI` = 4 (irq-apple-aic.c:84).
pub const AIC_EVENT_TYPE_IPI: u32 = 4;
/// `AIC_EVENT_IPI_OTHER` = 1 (irq-apple-aic.c:85); the only IPI number `aic_handle_irq` handles (:417).
pub const AIC_EVENT_IPI_OTHER: u32 = 1;
/// `AIC_EVENT_IPI_SELF` = 2 (irq-apple-aic.c:86); unused ("the second "self" IPI is unused", :35).
pub const AIC_EVENT_IPI_SELF: u32 = 2;
/// `AIC_IPI_SEND` (irq-apple-aic.c:88), written at :843.
pub const AIC_IPI_SEND: u32 = 0x2008;
/// `AIC_IPI_ACK` (irq-apple-aic.c:89), written at :825 and :907.
pub const AIC_IPI_ACK: u32 = 0x200c;
/// `AIC_IPI_MASK_SET` (irq-apple-aic.c:90), written at :909 and :911.
pub const AIC_IPI_MASK_SET: u32 = 0x2024;
/// `AIC_IPI_MASK_CLR` (irq-apple-aic.c:91), written at :835 and :912.
pub const AIC_IPI_MASK_CLR: u32 = 0x2028;
/// `AIC_IPI_OTHER` = BIT(0) (irq-apple-aic.c:95).
pub const AIC_IPI_OTHER: u32 = 1 << 0;
/// `AIC_IPI_SELF` = BIT(31) (irq-apple-aic.c:96).
pub const AIC_IPI_SELF: u32 = 1 << 31;
/// `AIC_TARGET_CPU` (irq-apple-aic.c:98): one u32 per IRQ, a CPU bit mask (:459, :1081).
pub const AIC_TARGET_CPU: u32 = 0x3000;
/// `AIC_CPU_IPI_SET(cpu)` (irq-apple-aic.c:100). Declared, never used by the C.
pub const fn aic_cpu_ipi_set(cpu: u32) -> u32 {
    0x5008 + (cpu << 7)
}
/// `AIC_CPU_IPI_CLR(cpu)` (irq-apple-aic.c:101). Declared, never used by the C.
pub const fn aic_cpu_ipi_clr(cpu: u32) -> u32 {
    0x500c + (cpu << 7)
}
/// `AIC_CPU_IPI_MASK_SET(cpu)` (irq-apple-aic.c:102). Declared, never used by the C.
pub const fn aic_cpu_ipi_mask_set(cpu: u32) -> u32 {
    0x5024 + (cpu << 7)
}
/// `AIC_CPU_IPI_MASK_CLR(cpu)` (irq-apple-aic.c:103). Declared, never used by the C.
pub const fn aic_cpu_ipi_mask_clr(cpu: u32) -> u32 {
    0x5028 + (cpu << 7)
}
/// `AIC_MAX_IRQ` = 0x400 (irq-apple-aic.c:105): v1's `max_irq` (:1001).
pub const AIC_MAX_IRQ: u32 = 0x400;

// ---- AIC v2 / v3 (MMIO) ----

/// `AIC2_VERSION` (irq-apple-aic.c:111). Declared, never used by the C.
pub const AIC2_VERSION: u32 = 0x0000;
/// `AIC2_VERSION_VER` = GENMASK(7, 0) (irq-apple-aic.c:112). Declared, never used by the C.
pub const AIC2_VERSION_VER: u32 = genmask(7, 0);
/// `AIC2_INFO1` (irq-apple-aic.c:114), read at :1014.
pub const AIC2_INFO1: u32 = 0x0004;
/// `AIC2_INFO1_NR_IRQ` = GENMASK(15, 0) (irq-apple-aic.c:115).
pub const AIC2_INFO1_NR_IRQ: u32 = genmask(15, 0);
/// `AIC2_INFO1_LAST_DIE` = GENMASK(27, 24) (irq-apple-aic.c:116); `nr_die` is this plus one (:1019).
pub const AIC2_INFO1_LAST_DIE: u32 = genmask(27, 24);
/// `AIC2_INFO2` (irq-apple-aic.c:118). Declared, never used by the C.
pub const AIC2_INFO2: u32 = 0x0008;
/// `AIC2_INFO3` (irq-apple-aic.c:120), read at :1015.
pub const AIC2_INFO3: u32 = 0x000c;
/// `AIC2_INFO3_MAX_IRQ` = GENMASK(15, 0) (irq-apple-aic.c:121).
pub const AIC2_INFO3_MAX_IRQ: u32 = genmask(15, 0);
/// `AIC2_INFO3_MAX_DIE` = GENMASK(27, 24) (irq-apple-aic.c:122).
pub const AIC2_INFO3_MAX_DIE: u32 = genmask(27, 24);
/// `AIC2_RESET` (irq-apple-aic.c:124). Declared, never used by the C.
pub const AIC2_RESET: u32 = 0x0010;
/// `AIC2_RESET_RESET` = BIT(0) (irq-apple-aic.c:125). Declared, never used by the C.
pub const AIC2_RESET_RESET: u32 = 1 << 0;
/// `AIC2_CONFIG` (irq-apple-aic.c:127), read-modify-written at :1086-1089.
pub const AIC2_CONFIG: u32 = 0x0014;
/// `AIC2_CONFIG_ENABLE` = BIT(0) (irq-apple-aic.c:128).
pub const AIC2_CONFIG_ENABLE: u32 = 1 << 0;
/// `AIC2_CONFIG_PREFER_PCPU` = BIT(28) (irq-apple-aic.c:129). Declared, never used by the C.
pub const AIC2_CONFIG_PREFER_PCPU: u32 = 1 << 28;
/// `AIC2_TIMEOUT` (irq-apple-aic.c:131). Declared, never used by the C.
pub const AIC2_TIMEOUT: u32 = 0x0028;
/// `AIC2_CLUSTER_PRIO` (irq-apple-aic.c:132). Declared, never used by the C.
pub const AIC2_CLUSTER_PRIO: u32 = 0x0030;
/// `AIC2_DELAY_GROUPS` (irq-apple-aic.c:133). Declared, never used by the C.
pub const AIC2_DELAY_GROUPS: u32 = 0x0100;
/// `AIC2_IRQ_CFG` (irq-apple-aic.c:135): start of the per-die blocks on v2 (`aic2_info.irq_cfg`, :294).
pub const AIC2_IRQ_CFG: u32 = 0x2000;
/// `AIC3_IRQ_CFG` (irq-apple-aic.c:138): start of the per-die blocks on v3 (`aic3_info.irq_cfg`, :303).
pub const AIC3_IRQ_CFG: u32 = 0x10000;
/// `AIC2_IRQ_CFG_TARGET` = GENMASK(3, 0) (irq-apple-aic.c:160). Declared, never used by the C.
pub const AIC2_IRQ_CFG_TARGET: u32 = genmask(3, 0);
/// `AIC2_IRQ_CFG_DELAY_IDX` = GENMASK(7, 5) (irq-apple-aic.c:161). Declared, never used by the C.
pub const AIC2_IRQ_CFG_DELAY_IDX: u32 = genmask(7, 5);

/// `MASK_REG(x)` = 4 * (x >> 5) (irq-apple-aic.c:163): byte offset of the bit-array word that holds IRQ `x`.
pub const fn mask_reg(x: u32) -> u32 {
    4 * (x >> 5)
}

/// `MASK_BIT(x)` = BIT(x & GENMASK(4, 0)) (irq-apple-aic.c:164): IRQ `x`'s bit in that word.
pub const fn mask_bit(x: u32) -> u32 {
    1 << (x & genmask(4, 0))
}

/// `BITS_TO_U32(nr)` = DIV_ROUND_UP(nr, 32) (include/linux/bitops.h:13), used for the bulk init at irq-apple-aic.c:1075-1077.
pub const fn bits_to_u32(nr: u32) -> u32 {
    nr.div_ceil(32)
}

// ---- Implementation-defined system register fields ----

/// `IPI_RR_CPU` = GENMASK(7, 0) (irq-apple-aic.c:173).
pub const IPI_RR_CPU: u32 = genmask(7, 0);
/// `IPI_RR_CLUSTER` = GENMASK(23, 16), "only used for the GLOBAL register" (irq-apple-aic.c:174-175).
pub const IPI_RR_CLUSTER: u32 = genmask(23, 16);
/// `IPI_RR_TYPE` = GENMASK(29, 28) (irq-apple-aic.c:176). The C never sets it, so it sends type 0, `IPI_RR_IMMEDIATE`.
pub const IPI_RR_TYPE: u32 = genmask(29, 28);
/// `IPI_RR_IMMEDIATE` = 0 (irq-apple-aic.c:177).
pub const IPI_RR_IMMEDIATE: u32 = 0;
/// `IPI_RR_RETRACT` = 1 (irq-apple-aic.c:178). Declared, never used by the C.
pub const IPI_RR_RETRACT: u32 = 1;
/// `IPI_RR_DEFERRED` = 2 (irq-apple-aic.c:179). Declared, never used by the C.
pub const IPI_RR_DEFERRED: u32 = 2;
/// `IPI_RR_NOWAKE` = 3 (irq-apple-aic.c:180). Declared, never used by the C.
pub const IPI_RR_NOWAKE: u32 = 3;
/// `IPI_SR_PENDING` = BIT(0) (irq-apple-aic.c:184): read at :575, written to acknowledge at :822 and :865.
pub const IPI_SR_PENDING: u64 = 1 << 0;
/// `VM_TMR_FIQ_ENABLE_V` = BIT(0) (irq-apple-aic.c:188).
pub const VM_TMR_FIQ_ENABLE_V: u64 = 1 << 0;
/// `VM_TMR_FIQ_ENABLE_P` = BIT(1) (irq-apple-aic.c:189).
pub const VM_TMR_FIQ_ENABLE_P: u64 = 1 << 1;
/// `UPMCR0_IMODE` = GENMASK(18, 16) (irq-apple-aic.c:196).
pub const UPMCR0_IMODE: u64 = genmask(18, 16) as u64;
/// `UPMCR0_IMODE_OFF` = 0 (irq-apple-aic.c:197).
pub const UPMCR0_IMODE_OFF: u64 = 0;
/// `UPMCR0_IMODE_AIC` = 2 (irq-apple-aic.c:198). Declared, never used by the C.
pub const UPMCR0_IMODE_AIC: u64 = 2;
/// `UPMCR0_IMODE_HALT` = 3 (irq-apple-aic.c:199). Declared, never used by the C.
pub const UPMCR0_IMODE_HALT: u64 = 3;
/// `UPMCR0_IMODE_FIQ` = 4 (irq-apple-aic.c:200).
pub const UPMCR0_IMODE_FIQ: u64 = 4;
/// `UPMSR_IACT` = BIT(0) (irq-apple-aic.c:204).
pub const UPMSR_IACT: u64 = 1 << 0;
/// `PMCR0_IMODE` = GENMASK(10, 8) (arch/arm64/include/asm/apple_m1_pmu.h:24, included at irq-apple-aic.c:59).
pub const PMCR0_IMODE: u64 = genmask(10, 8) as u64;
/// `PMCR0_IMODE_OFF` = 0 (asm/apple_m1_pmu.h:25), used at irq-apple-aic.c:883.
pub const PMCR0_IMODE_OFF: u64 = 0;
/// `PMCR0_IMODE_FIQ` = 4 (asm/apple_m1_pmu.h:29), used at irq-apple-aic.c:601.
pub const PMCR0_IMODE_FIQ: u64 = 4;
/// `PMCR0_IACT` = BIT(11) (asm/apple_m1_pmu.h:30), used at irq-apple-aic.c:600 and :882.
pub const PMCR0_IACT: u64 = 1 << 11;
/// `ICH_HCR_EL2_En` = bit 0 (arch/arm64/tools/sysreg:5238, `Field 0 En` of ICH_HCR_EL2), used at irq-apple-aic.c:429.
pub const ICH_HCR_EL2_EN: u64 = 1 << 0;

/// `ARCH_TIMER_CTRL_ENABLE` = 1 << 0 (include/clocksource/arm_arch_timer.h:12), used by `TIMER_FIRING` (irq-apple-aic.c:552-555).
pub const ARCH_TIMER_CTRL_ENABLE: u64 = 1 << 0;
/// `ARCH_TIMER_CTRL_IT_MASK` = 1 << 1 (arm_arch_timer.h:13), set by `aic_init_cpu` (irq-apple-aic.c:868-869).
pub const ARCH_TIMER_CTRL_IT_MASK: u64 = 1 << 1;
/// `ARCH_TIMER_CTRL_IT_STAT` = 1 << 2 (arm_arch_timer.h:14).
pub const ARCH_TIMER_CTRL_IT_STAT: u64 = 1 << 2;

/// `TIMER_FIRING(x)` (irq-apple-aic.c:552-555): enabled, not masked, and its condition met.
pub const fn timer_firing(ctl: u64) -> bool {
    (ctl & (ARCH_TIMER_CTRL_ENABLE | ARCH_TIMER_CTRL_IT_MASK | ARCH_TIMER_CTRL_IT_STAT))
        == (ARCH_TIMER_CTRL_ENABLE | ARCH_TIMER_CTRL_IT_STAT)
}

/// `MPIDR_CPU(x)` = affinity level 0 = bits 7:0 (irq-apple-aic.c:207; `MPIDR_AFFINITY_LEVEL`, asm/cputype.h:14-22).
pub const fn mpidr_cpu(mpidr: u64) -> u64 {
    mpidr & 0xff
}

/// `MPIDR_CLUSTER(x)` = affinity level 1 = bits 15:8 (irq-apple-aic.c:208; asm/cputype.h:14-22).
pub const fn mpidr_cluster(mpidr: u64) -> u64 {
    (mpidr >> 8) & 0xff
}

// ---- Device-tree binding values ----

/// `AIC_IRQ` = 0, first interrupt cell of a hardware IRQ (include/dt-bindings/interrupt-controller/apple-aic.h:7).
pub const DT_AIC_IRQ: u32 = 0;
/// `AIC_FIQ` = 1, first interrupt cell of a FIQ (apple-aic.h:8).
pub const DT_AIC_FIQ: u32 = 1;
/// `AIC_TMR_HV_PHYS` = 0 (apple-aic.h:10).
pub const AIC_TMR_HV_PHYS: u32 = 0;
/// `AIC_TMR_HV_VIRT` = 1 (apple-aic.h:11).
pub const AIC_TMR_HV_VIRT: u32 = 1;
/// `AIC_TMR_GUEST_PHYS` = 2 (apple-aic.h:12).
pub const AIC_TMR_GUEST_PHYS: u32 = 2;
/// `AIC_TMR_GUEST_VIRT` = 3 (apple-aic.h:13).
pub const AIC_TMR_GUEST_VIRT: u32 = 3;
/// `AIC_CPU_PMU_E` = 4 (apple-aic.h:14).
pub const AIC_CPU_PMU_E: u32 = 4;
/// `AIC_CPU_PMU_P` = 5 (apple-aic.h:15).
pub const AIC_CPU_PMU_P: u32 = 5;

/// `IRQ_TYPE_EDGE_RISING` = 1 (include/dt-bindings/interrupt-controller/irq.h:14; include/linux/irq.h:82).
pub const IRQ_TYPE_EDGE_RISING: u32 = 1;
/// `IRQ_TYPE_LEVEL_HIGH` = 4 (dt-bindings irq.h:17; include/linux/irq.h:85).
pub const IRQ_TYPE_LEVEL_HIGH: u32 = 4;
/// `IRQ_TYPE_SENSE_MASK` = 0xf (include/linux/irq.h:88), applied to the flags cell at irq-apple-aic.c:746.
pub const IRQ_TYPE_SENSE_MASK: u32 = 0xf;

// ---- FIQ hwirq indices (enum fiq_hwirq, irq-apple-aic.c:228-239) ----

/// `AIC_TMR_EL0_PHYS` = `AIC_TMR_HV_PHYS` (irq-apple-aic.c:230): the timer behind `cntp_ctl_el0`.
pub const FIQ_TMR_EL0_PHYS: u32 = AIC_TMR_HV_PHYS;
/// `AIC_TMR_EL0_VIRT` = `AIC_TMR_HV_VIRT` (irq-apple-aic.c:231): the timer behind `cntv_ctl_el0`.
pub const FIQ_TMR_EL0_VIRT: u32 = AIC_TMR_HV_VIRT;
/// `AIC_TMR_EL02_PHYS` = `AIC_TMR_GUEST_PHYS` (irq-apple-aic.c:232): `CNTP_CTL_EL02`.
pub const FIQ_TMR_EL02_PHYS: u32 = AIC_TMR_GUEST_PHYS;
/// `AIC_TMR_EL02_VIRT` = `AIC_TMR_GUEST_VIRT` (irq-apple-aic.c:233): `CNTV_CTL_EL02`.
pub const FIQ_TMR_EL02_VIRT: u32 = AIC_TMR_GUEST_VIRT;
/// `AIC_CPU_PMU_Effi` = `AIC_CPU_PMU_E` (irq-apple-aic.c:234).
pub const FIQ_CPU_PMU_EFFI: u32 = AIC_CPU_PMU_E;
/// `AIC_CPU_PMU_Perf` = `AIC_CPU_PMU_P` (irq-apple-aic.c:235).
pub const FIQ_CPU_PMU_PERF: u32 = AIC_CPU_PMU_P;
/// `AIC_VGIC_MI` = 6, "No need for this to be discovered from DT" (irq-apple-aic.c:236-237).
pub const FIQ_VGIC_MI: u32 = 6;
/// `AIC_NR_FIQ` = 7 (irq-apple-aic.c:238).
pub const AIC_NR_FIQ: u32 = 7;

/// `AIC_NR_SWIPI` = 32 (irq-apple-aic.c:217): the virtual IPIs multiplexed onto one hardware IPI (:850, :854).
pub const AIC_NR_SWIPI: u32 = 32;
