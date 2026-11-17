// SPDX-License-Identifier: GPL-2.0-or-later
//! Differential test of the register constants: ours in `src/regs.rs` against the same constants in the original they were
//! ported from, AsahiLinux/linux (branch `asahi`) at the pinned revision below.
//!
//! Run it on a machine that holds a clone of the original (the path is in `README.md`, "Provenance"):
//!
//! ```text
//! LUCINA_ASAHI_LINUX=/home/user/Projects/ReverseEngineering/asahi-m4/linux \
//!   cargo test --locked -p lucina-apple-aic --test original_constants
//! ```
//!
//! With `LUCINA_ASAHI_LINUX` unset the comparisons are skipped and the test passes, since CI and the other hosts have no
//! clone. The classification of our constants is checked either way; it needs only this repository.
//!
//! Nothing of the original is kept here: the tables name theirs and ours, and the original text is read at test time with
//! `git show` from the object store. The original is GPL-2.0-or-later; reading it and comparing two numbers takes no code
//! into this tree.
//!
//! # How the original's value is obtained
//!
//! `#define NAME value` lines and enum members are read out of the original; the right-hand side is expanded the way the C
//! preprocessor would (object-like and function-like macros, parameters bound to the probe's argument) and evaluated with
//! C's precedence. The forms implemented are the ones these files use: hex and decimal literals with C suffixes, `<< >> & |
//! ^ + - * / % ~ ! ?: == != && ||`, casts, `sizeof` of the kernel's fixed-width types, `GENMASK`/`BIT`/`UL`. Anything else is
//! a hard error naming the text it choked on, never a silent pass.
//!
//! Two kernel macros are given their meaning rather than their body, because their body is compile-time argument checking
//! around that meaning:
//!
//! - `GENMASK(h, l)`: the bits `h..l` set. `include/linux/bits.h:46-51` computes exactly that for `unsigned long`
//!   (`type_max(t) << (l) & type_max(t) >> (BITS_PER_TYPE(t) - 1 - (h))`, plus `GENMASK_INPUT_CHECK` which contributes 0), so
//!   the 64-bit width of `unsigned long` is kept; every value compared below fits in the `u32`/`u64` of our own constant, so
//!   the width cannot change a comparison.
//! - `BIT(n)`: `1 << n`. Where `BIT()` is defined at this revision was not found among the files this test reads
//!   (`include/linux/bits.h` and `include/linux/bitops.h` use it, `include/uapi/linux/const.h:25-30` defines only `_UL()` and
//!   `_BITUL()`), so this is the meaning the kernel gives it, taken from its uses and not from its body (not read). A shift
//!   of 64 or more yields 0 instead of panicking; nothing compared here shifts that far.
//!
//! The first `#define` of a name wins, which is the `!__ASSEMBLY__` branch of the two headers that carry both
//! (`include/linux/bits.h:51` and `:84`).

use std::collections::BTreeMap;
use std::process::Command;

/// The revision `src/regs.rs` is ported from and names in its header: AsahiLinux/linux branch `asahi`, commit
/// `6ba0637d1ed9a2e3639dd3fc9eca6cd0483a6434`, dated 2026-11-05. Pinned, so the comparison cannot move under us.
const ORIGINAL_REV: &str = "6ba0637d1ed9";

/// The environment variable naming a git clone of the original. Unset means: skip.
const CLONE_VAR: &str = "LUCINA_ASAHI_LINUX";

/// Our file, as the messages name it: a mismatch points at both sides and both lines.
const OURS_PATH: &str = "drivers/lucina-apple-aic/src/regs.rs";

/// An original file this test reads at the pinned revision.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Orig {
    Aic,
    Pmu,
    Timer,
    DtAic,
    DtIrq,
    LinuxIrq,
    Cputype,
    Bitops,
    Bits,
    UapiConst,
    Sysreg,
}

impl Orig {
    fn path(self) -> &'static str {
        match self {
            Orig::Aic => "drivers/irqchip/irq-apple-aic.c",
            Orig::Pmu => "arch/arm64/include/asm/apple_m1_pmu.h",
            Orig::Timer => "include/clocksource/arm_arch_timer.h",
            Orig::DtAic => "include/dt-bindings/interrupt-controller/apple-aic.h",
            Orig::DtIrq => "include/dt-bindings/interrupt-controller/irq.h",
            Orig::LinuxIrq => "include/linux/irq.h",
            Orig::Cputype => "arch/arm64/include/asm/cputype.h",
            Orig::Bitops => "include/linux/bitops.h",
            Orig::Bits => "include/linux/bits.h",
            Orig::UapiConst => "include/uapi/linux/const.h",
            Orig::Sysreg => "arch/arm64/tools/sysreg",
        }
    }

    /// Read, and parsed for `#define`s and enum members, in this order: the driver itself, then the headers it and our file
    /// lean on, which is the order our `src/regs.rs` cites them in.
    const PARSED: &'static [Orig] = &[
        Orig::Aic,
        Orig::Pmu,
        Orig::Timer,
        Orig::DtAic,
        Orig::DtIrq,
        Orig::LinuxIrq,
        Orig::Cputype,
        Orig::Bitops,
        Orig::Bits,
        Orig::UapiConst,
    ];
}

// ---------------------------------------------------------------------------
// The tables: every constant of ours, classified.
// ---------------------------------------------------------------------------

/// Our constant, the original file, the original's name. The two values must be equal. Names differ only where ours says
/// more than the C's (`DT_AIC_IRQ` for the binding's `AIC_IRQ`, `FIQ_*` for the C's `enum fiq_hwirq` members).
const SCALARS: &[(&str, Orig, &str)] = &[
    // AIC v1 registers and fields, `irq-apple-aic.c:71-105`.
    ("AIC_INFO", Orig::Aic, "AIC_INFO"),
    ("AIC_INFO_NR_IRQ", Orig::Aic, "AIC_INFO_NR_IRQ"),
    ("AIC_CONFIG", Orig::Aic, "AIC_CONFIG"),
    ("AIC_WHOAMI", Orig::Aic, "AIC_WHOAMI"),
    ("AIC_EVENT", Orig::Aic, "AIC_EVENT"),
    ("AIC_EVENT_DIE", Orig::Aic, "AIC_EVENT_DIE"),
    ("AIC_EVENT_TYPE", Orig::Aic, "AIC_EVENT_TYPE"),
    ("AIC_EVENT_NUM", Orig::Aic, "AIC_EVENT_NUM"),
    ("AIC_EVENT_TYPE_FIQ", Orig::Aic, "AIC_EVENT_TYPE_FIQ"),
    ("AIC_EVENT_TYPE_IRQ", Orig::Aic, "AIC_EVENT_TYPE_IRQ"),
    ("AIC_EVENT_TYPE_IPI", Orig::Aic, "AIC_EVENT_TYPE_IPI"),
    ("AIC_EVENT_IPI_OTHER", Orig::Aic, "AIC_EVENT_IPI_OTHER"),
    ("AIC_EVENT_IPI_SELF", Orig::Aic, "AIC_EVENT_IPI_SELF"),
    ("AIC_IPI_SEND", Orig::Aic, "AIC_IPI_SEND"),
    ("AIC_IPI_ACK", Orig::Aic, "AIC_IPI_ACK"),
    ("AIC_IPI_MASK_SET", Orig::Aic, "AIC_IPI_MASK_SET"),
    ("AIC_IPI_MASK_CLR", Orig::Aic, "AIC_IPI_MASK_CLR"),
    ("AIC_IPI_OTHER", Orig::Aic, "AIC_IPI_OTHER"),
    ("AIC_IPI_SELF", Orig::Aic, "AIC_IPI_SELF"),
    ("AIC_TARGET_CPU", Orig::Aic, "AIC_TARGET_CPU"),
    ("AIC_MAX_IRQ", Orig::Aic, "AIC_MAX_IRQ"),
    // AIC v2 and v3 registers and fields, `irq-apple-aic.c:111-161`. The M4's `t8132-aic3` needs no constant of its own: the
    // v3 shape is `AIC3_IRQ_CFG` (compared below) plus the counts the driver reads from `AIC2_INFO1`/`AIC2_INFO3` at run
    // time, which this crate takes as inputs (README.md:85-86).
    ("AIC2_VERSION", Orig::Aic, "AIC2_VERSION"),
    ("AIC2_VERSION_VER", Orig::Aic, "AIC2_VERSION_VER"),
    ("AIC2_INFO1", Orig::Aic, "AIC2_INFO1"),
    ("AIC2_INFO1_NR_IRQ", Orig::Aic, "AIC2_INFO1_NR_IRQ"),
    ("AIC2_INFO1_LAST_DIE", Orig::Aic, "AIC2_INFO1_LAST_DIE"),
    ("AIC2_INFO2", Orig::Aic, "AIC2_INFO2"),
    ("AIC2_INFO3", Orig::Aic, "AIC2_INFO3"),
    ("AIC2_INFO3_MAX_IRQ", Orig::Aic, "AIC2_INFO3_MAX_IRQ"),
    ("AIC2_INFO3_MAX_DIE", Orig::Aic, "AIC2_INFO3_MAX_DIE"),
    ("AIC2_RESET", Orig::Aic, "AIC2_RESET"),
    ("AIC2_RESET_RESET", Orig::Aic, "AIC2_RESET_RESET"),
    ("AIC2_CONFIG", Orig::Aic, "AIC2_CONFIG"),
    ("AIC2_CONFIG_ENABLE", Orig::Aic, "AIC2_CONFIG_ENABLE"),
    (
        "AIC2_CONFIG_PREFER_PCPU",
        Orig::Aic,
        "AIC2_CONFIG_PREFER_PCPU",
    ),
    ("AIC2_TIMEOUT", Orig::Aic, "AIC2_TIMEOUT"),
    ("AIC2_CLUSTER_PRIO", Orig::Aic, "AIC2_CLUSTER_PRIO"),
    ("AIC2_DELAY_GROUPS", Orig::Aic, "AIC2_DELAY_GROUPS"),
    ("AIC2_IRQ_CFG", Orig::Aic, "AIC2_IRQ_CFG"),
    ("AIC3_IRQ_CFG", Orig::Aic, "AIC3_IRQ_CFG"),
    ("AIC2_IRQ_CFG_TARGET", Orig::Aic, "AIC2_IRQ_CFG_TARGET"),
    (
        "AIC2_IRQ_CFG_DELAY_IDX",
        Orig::Aic,
        "AIC2_IRQ_CFG_DELAY_IDX",
    ),
    // The FIQ request/status and uncore PMC fields, `irq-apple-aic.c:173-204`.
    ("IPI_RR_CPU", Orig::Aic, "IPI_RR_CPU"),
    ("IPI_RR_CLUSTER", Orig::Aic, "IPI_RR_CLUSTER"),
    ("IPI_RR_TYPE", Orig::Aic, "IPI_RR_TYPE"),
    ("IPI_RR_IMMEDIATE", Orig::Aic, "IPI_RR_IMMEDIATE"),
    ("IPI_RR_RETRACT", Orig::Aic, "IPI_RR_RETRACT"),
    ("IPI_RR_DEFERRED", Orig::Aic, "IPI_RR_DEFERRED"),
    ("IPI_RR_NOWAKE", Orig::Aic, "IPI_RR_NOWAKE"),
    ("IPI_SR_PENDING", Orig::Aic, "IPI_SR_PENDING"),
    ("VM_TMR_FIQ_ENABLE_V", Orig::Aic, "VM_TMR_FIQ_ENABLE_V"),
    ("VM_TMR_FIQ_ENABLE_P", Orig::Aic, "VM_TMR_FIQ_ENABLE_P"),
    ("UPMCR0_IMODE", Orig::Aic, "UPMCR0_IMODE"),
    ("UPMCR0_IMODE_OFF", Orig::Aic, "UPMCR0_IMODE_OFF"),
    ("UPMCR0_IMODE_AIC", Orig::Aic, "UPMCR0_IMODE_AIC"),
    ("UPMCR0_IMODE_HALT", Orig::Aic, "UPMCR0_IMODE_HALT"),
    ("UPMCR0_IMODE_FIQ", Orig::Aic, "UPMCR0_IMODE_FIQ"),
    ("UPMSR_IACT", Orig::Aic, "UPMSR_IACT"),
    // The P-core PMU fields: `arch/arm64/include/asm/apple_m1_pmu.h`, which the driver includes at `:59`.
    ("PMCR0_IMODE", Orig::Pmu, "PMCR0_IMODE"),
    ("PMCR0_IMODE_OFF", Orig::Pmu, "PMCR0_IMODE_OFF"),
    ("PMCR0_IMODE_FIQ", Orig::Pmu, "PMCR0_IMODE_FIQ"),
    ("PMCR0_IACT", Orig::Pmu, "PMCR0_IACT"),
    // The arch timer control bits: `include/clocksource/arm_arch_timer.h:12-14`.
    (
        "ARCH_TIMER_CTRL_ENABLE",
        Orig::Timer,
        "ARCH_TIMER_CTRL_ENABLE",
    ),
    (
        "ARCH_TIMER_CTRL_IT_MASK",
        Orig::Timer,
        "ARCH_TIMER_CTRL_IT_MASK",
    ),
    (
        "ARCH_TIMER_CTRL_IT_STAT",
        Orig::Timer,
        "ARCH_TIMER_CTRL_IT_STAT",
    ),
    // The device-tree binding: `include/dt-bindings/interrupt-controller/apple-aic.h:7-15`.
    ("DT_AIC_IRQ", Orig::DtAic, "AIC_IRQ"),
    ("DT_AIC_FIQ", Orig::DtAic, "AIC_FIQ"),
    ("AIC_TMR_HV_PHYS", Orig::DtAic, "AIC_TMR_HV_PHYS"),
    ("AIC_TMR_HV_VIRT", Orig::DtAic, "AIC_TMR_HV_VIRT"),
    ("AIC_TMR_GUEST_PHYS", Orig::DtAic, "AIC_TMR_GUEST_PHYS"),
    ("AIC_TMR_GUEST_VIRT", Orig::DtAic, "AIC_TMR_GUEST_VIRT"),
    ("AIC_CPU_PMU_E", Orig::DtAic, "AIC_CPU_PMU_E"),
    ("AIC_CPU_PMU_P", Orig::DtAic, "AIC_CPU_PMU_P"),
    // Trigger types, from the DT binding (`irq.h:14` and `:17`). `include/linux/irq.h:82` and `:85` spell the same two
    // numbers as enum members; the binding is what a device tree carries, so the binding is what is compared.
    ("IRQ_TYPE_EDGE_RISING", Orig::DtIrq, "IRQ_TYPE_EDGE_RISING"),
    ("IRQ_TYPE_LEVEL_HIGH", Orig::DtIrq, "IRQ_TYPE_LEVEL_HIGH"),
    // The sense mask has no DT-binding define at all; it is the enum member at `include/linux/irq.h:88`, which our
    // `src/regs.rs:238` cites for the flags cell.
    ("IRQ_TYPE_SENSE_MASK", Orig::LinuxIrq, "IRQ_TYPE_SENSE_MASK"),
    // The FIQ hwirq indices: `enum fiq_hwirq`, `irq-apple-aic.c:228-239`. Ours say `FIQ_`, the C names a timer by its
    // purpose (`AIC_TMR_*`) and the PMU by its register (`AIC_CPU_PMU_Effi`, `AIC_CPU_PMU_Perf`); `AIC_VGIC_MI` and
    // `AIC_NR_FIQ` have no `= ...` in the C at all and are the count of what comes before them.
    ("FIQ_TMR_EL0_PHYS", Orig::Aic, "AIC_TMR_EL0_PHYS"),
    ("FIQ_TMR_EL0_VIRT", Orig::Aic, "AIC_TMR_EL0_VIRT"),
    ("FIQ_TMR_EL02_PHYS", Orig::Aic, "AIC_TMR_EL02_PHYS"),
    ("FIQ_TMR_EL02_VIRT", Orig::Aic, "AIC_TMR_EL02_VIRT"),
    ("FIQ_CPU_PMU_EFFI", Orig::Aic, "AIC_CPU_PMU_Effi"),
    ("FIQ_CPU_PMU_PERF", Orig::Aic, "AIC_CPU_PMU_Perf"),
    ("FIQ_VGIC_MI", Orig::Aic, "AIC_VGIC_MI"),
    ("AIC_NR_FIQ", Orig::Aic, "AIC_NR_FIQ"),
    // The virtual IPI count, `irq-apple-aic.c:217`.
    ("AIC_NR_SWIPI", Orig::Aic, "AIC_NR_SWIPI"),
];

/// One `MACROS` entry: ours, the original file, the original macro, the parameters held fixed, the probe values.
type MacroProbe = (
    &'static str,
    Orig,
    &'static str,
    &'static [(&'static str, u64)],
    &'static [u64],
);

/// A constant of ours that is a function of one argument, compared against the original's function-like macro at probe
/// arguments: ours, the original file, the original macro, the parameters held fixed, the probe values for the free
/// parameter (which must be exactly one, and is what our `const fn` is called with).
///
/// Every probe is a plain non-negative integer that fits in `u32`, so the C macros that do not parenthesise their parameter
/// (`MPIDR_AFFINITY_LEVEL` at `arch/arm64/include/asm/cputype.h:21-22`) cannot be taken the wrong way by a probe.
const MACROS: &[MacroProbe] = &[
    // `AIC_CPU_IPI_SET/CLR/MASK_SET/MASK_CLR(cpu)` (`:100-103`) over our `aic_cpu_ipi_*`. Eight is more CPUs than any Apple
    // part; the C bounds the argument no more than we do.
    (
        "aic_cpu_ipi_set",
        Orig::Aic,
        "AIC_CPU_IPI_SET",
        &[],
        &[0, 1, 2, 3, 4, 5, 6, 7, 8],
    ),
    (
        "aic_cpu_ipi_clr",
        Orig::Aic,
        "AIC_CPU_IPI_CLR",
        &[],
        &[0, 1, 2, 3, 4, 5, 6, 7, 8],
    ),
    (
        "aic_cpu_ipi_mask_set",
        Orig::Aic,
        "AIC_CPU_IPI_MASK_SET",
        &[],
        &[0, 1, 2, 3, 4, 5, 6, 7, 8],
    ),
    (
        "aic_cpu_ipi_mask_clr",
        Orig::Aic,
        "AIC_CPU_IPI_MASK_CLR",
        &[],
        &[0, 1, 2, 3, 4, 5, 6, 7, 8],
    ),
    // `MASK_REG(x)` and `MASK_BIT(x)` (`:163-164`) over our `mask_reg` / `mask_bit`: probes straddle the 32-bit word
    // boundary, run past `AIC_MAX_IRQ` (0x400) and past the 896 IRQs of the M1 (`:14`).
    (
        "mask_reg",
        Orig::Aic,
        "MASK_REG",
        &[],
        &[0, 1, 31, 32, 33, 63, 64, 895, 896, 1023, 4096],
    ),
    (
        "mask_bit",
        Orig::Aic,
        "MASK_BIT",
        &[],
        &[0, 1, 4, 31, 32, 33, 63, 64, 895, 1000],
    ),
    // `BITS_TO_U32(nr)` (`include/linux/bitops.h:13`) over our `bits_to_u32`, at and around the multiples of 32 the bulk
    // init walks (`irq-apple-aic.c:1075-1077`). Its body expands through `__KERNEL_DIV_ROUND_UP`
    // (`include/uapi/linux/const.h:51`) and `BITS_PER_TYPE(u32)` (`include/linux/bits.h:12-13`), which is why those two
    // headers are read too.
    (
        "bits_to_u32",
        Orig::Bitops,
        "BITS_TO_U32",
        &[],
        &[0, 1, 31, 32, 33, 64, 65, 896, 1000, 4096],
    ),
    // `MPIDR_CPU(x)` / `MPIDR_CLUSTER(x)` (`:207-208`), i.e. `MPIDR_AFFINITY_LEVEL(x, 0)` and `(x, 1)`. The probes are an
    // affinity0-only value, a cluster-only value, and MPIDR-shaped words with the UP (bit 30) and MT (bit 24) flags set, as
    // `aic_init_cpu` reads them (`:861`).
    (
        "mpidr_cpu",
        Orig::Cputype,
        "MPIDR_AFFINITY_LEVEL",
        &[("level", 0)],
        &[
            0,
            0xff,
            0x100,
            0x8100,
            0x4000_0000_0000_0011,
            0x0000_0080_0000_0011,
        ],
    ),
    (
        "mpidr_cluster",
        Orig::Cputype,
        "MPIDR_AFFINITY_LEVEL",
        &[("level", 1)],
        &[
            0,
            0xff,
            0x100,
            0x8100,
            0x4000_0000_0000_0011,
            0x0000_0080_0000_0011,
        ],
    ),
    // `TIMER_FIRING(x)` (`:552-555`) over our `timer_firing`; our `bool` compares as 0 or 1. The probes are the eight
    // combinations of the three control bits plus two words that carry other bits.
    (
        "timer_firing",
        Orig::Aic,
        "TIMER_FIRING",
        &[],
        &[0, 1, 2, 3, 4, 5, 6, 7, 8, 0xff, 0x100],
    ),
];

/// Ours whose counterpart in the original is not a `#define` at all. `how` says what it is and where the value comes from;
/// the test still reads the original and compares, by the route named in `kind`.
const NOT_A_DEFINE: &[(&str, NotDefine)] = &[(
    "ICH_HCR_EL2_EN",
    NotDefine::SysregField {
        reg: "ICH_HCR_EL2",
        field: "En",
        how: "bit 0 of ICH_HCR_EL2, written as the row `Field 0 En` of the system-register table \
              arch/arm64/tools/sysreg:5238 at this revision (the line our regs.rs:189 cites). The generated \
              `asm/sysreg.h` is build output and not in the tree, so the table itself is read and the mask built from the \
              row.",
    },
)];

/// Ours with no counterpart in the original at all, and why.
///
/// That is the whole list for this crate: `src/regs.rs` holds no constant of our own invention, and no AIC v3 or M4 addition.
/// The M4 has no register constant of its own because its numbers are read at run time — `nr_irq`, `max_irq` and the die
/// counts come from `AIC2_INFO1`/`AIC2_INFO3` (`README.md:85-86`, citing `irq-apple-aic.c:1014-1020`) — and v3 differs from
/// v2 in `AIC3_IRQ_CFG` alone (`:138`), which is compared above.
const OURS_ONLY: &[(&str, &str)] = &[
    (
        "genmask",
        "our `const fn` for the kernel's `GENMASK(h, l)` (include/linux/bits.h:51). No constant of the original carries it; \
         it is checked by every entry of SCALARS whose original is a `GENMASK(...)`, and by `AIC2_IRQ_CFG_TARGET` alone it \
         is checked at six different bit ranges.",
    ),
    (
        "field_get",
        "our `const fn` for `FIELD_GET(mask, v)`; the original uses that only inside `AIC_HWIRQ_IRQ`/`AIC_HWIRQ_DIE` \
         (irq-apple-aic.c:215-216), which is ported as src/hwirq.rs (Verus-checked) and not as a constant here.",
    ),
    (
        "field_prep",
        "our `const fn` for `FIELD_PREP(mask, v)`; the original uses that only inside `AIC_IRQ_HWIRQ`/`AIC_FIQ_HWIRQ` \
         (irq-apple-aic.c:210-214), ported as src/hwirq.rs.",
    ),
];

#[derive(Copy, Clone, Debug)]
enum NotDefine {
    /// A row of the system-register table: the field's mask is the counterpart.
    SysregField {
        reg: &'static str,
        field: &'static str,
        how: &'static str,
    },
}

// ---------------------------------------------------------------------------
// Our side: what `src/regs.rs` declares, and what each of those is worth.
// ---------------------------------------------------------------------------

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Ours {
    /// `pub const NAME: T = ...;`
    Scalar,
    /// `pub const fn NAME(...)`
    Func,
}

/// Every `pub const` and `pub const fn` of ours, with the line it is on. Parsed rather than listed, so a constant added to
/// the crate without a table entry above fails the test instead of going unclassified.
fn ours_declared() -> BTreeMap<String, (usize, Ours)> {
    let path = format!("{}/src/regs.rs", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).expect("our src/regs.rs is readable");
    let mut out = BTreeMap::new();
    for (i, line) in text.lines().enumerate() {
        let Some(rest) = line.strip_prefix("pub const ") else {
            continue;
        };
        let (kind, rest) = match rest.strip_prefix("fn ") {
            Some(r) => (Ours::Func, r),
            None => (Ours::Scalar, rest),
        };
        let end = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(rest.len());
        let name = &rest[..end];
        assert!(
            !name.is_empty(),
            "{path}:{}: no name after `pub const`",
            i + 1
        );
        if kind == Ours::Scalar {
            assert!(
                rest[end..].starts_with(':'),
                "{path}:{}: `{name}` is not a `pub const NAME: T = ...;` item",
                i + 1
            );
        } else {
            assert!(
                rest[end..].starts_with('('),
                "{path}:{}: `{name}` is not a `pub const fn NAME(...)` item",
                i + 1
            );
        }
        let fresh = out.insert(name.to_string(), (i + 1, kind));
        assert!(fresh.is_none(), "{path}: `{name}` is declared twice");
    }
    assert!(
        out.len() > 80,
        "{path}: only {} items parsed, the file moved?",
        out.len()
    );
    out
}

macro_rules! our_scalar_values {
    ($($name:ident),* $(,)?) => {
        /// The value of one of our scalar constants, by name. A name in this list that `src/regs.rs` does not declare is a
        /// compile error, so the list is checked against the crate too.
        fn our_value(name: &str) -> Option<u64> {
            $(
                if name == stringify!($name) {
                    return Some(lucina_apple_aic::regs::$name as u64);
                }
            )*
            None
        }
    };
}

our_scalar_values!(
    AIC_INFO,
    AIC_INFO_NR_IRQ,
    AIC_CONFIG,
    AIC_WHOAMI,
    AIC_EVENT,
    AIC_EVENT_DIE,
    AIC_EVENT_TYPE,
    AIC_EVENT_NUM,
    AIC_EVENT_TYPE_FIQ,
    AIC_EVENT_TYPE_IRQ,
    AIC_EVENT_TYPE_IPI,
    AIC_EVENT_IPI_OTHER,
    AIC_EVENT_IPI_SELF,
    AIC_IPI_SEND,
    AIC_IPI_ACK,
    AIC_IPI_MASK_SET,
    AIC_IPI_MASK_CLR,
    AIC_IPI_OTHER,
    AIC_IPI_SELF,
    AIC_TARGET_CPU,
    AIC_MAX_IRQ,
    AIC2_VERSION,
    AIC2_VERSION_VER,
    AIC2_INFO1,
    AIC2_INFO1_NR_IRQ,
    AIC2_INFO1_LAST_DIE,
    AIC2_INFO2,
    AIC2_INFO3,
    AIC2_INFO3_MAX_IRQ,
    AIC2_INFO3_MAX_DIE,
    AIC2_RESET,
    AIC2_RESET_RESET,
    AIC2_CONFIG,
    AIC2_CONFIG_ENABLE,
    AIC2_CONFIG_PREFER_PCPU,
    AIC2_TIMEOUT,
    AIC2_CLUSTER_PRIO,
    AIC2_DELAY_GROUPS,
    AIC2_IRQ_CFG,
    AIC3_IRQ_CFG,
    AIC2_IRQ_CFG_TARGET,
    AIC2_IRQ_CFG_DELAY_IDX,
    IPI_RR_CPU,
    IPI_RR_CLUSTER,
    IPI_RR_TYPE,
    IPI_RR_IMMEDIATE,
    IPI_RR_RETRACT,
    IPI_RR_DEFERRED,
    IPI_RR_NOWAKE,
    IPI_SR_PENDING,
    VM_TMR_FIQ_ENABLE_V,
    VM_TMR_FIQ_ENABLE_P,
    UPMCR0_IMODE,
    UPMCR0_IMODE_OFF,
    UPMCR0_IMODE_AIC,
    UPMCR0_IMODE_HALT,
    UPMCR0_IMODE_FIQ,
    UPMSR_IACT,
    PMCR0_IMODE,
    PMCR0_IMODE_OFF,
    PMCR0_IMODE_FIQ,
    PMCR0_IACT,
    ICH_HCR_EL2_EN,
    ARCH_TIMER_CTRL_ENABLE,
    ARCH_TIMER_CTRL_IT_MASK,
    ARCH_TIMER_CTRL_IT_STAT,
    DT_AIC_IRQ,
    DT_AIC_FIQ,
    AIC_TMR_HV_PHYS,
    AIC_TMR_HV_VIRT,
    AIC_TMR_GUEST_PHYS,
    AIC_TMR_GUEST_VIRT,
    AIC_CPU_PMU_E,
    AIC_CPU_PMU_P,
    IRQ_TYPE_EDGE_RISING,
    IRQ_TYPE_LEVEL_HIGH,
    IRQ_TYPE_SENSE_MASK,
    FIQ_TMR_EL0_PHYS,
    FIQ_TMR_EL0_VIRT,
    FIQ_TMR_EL02_PHYS,
    FIQ_TMR_EL02_VIRT,
    FIQ_CPU_PMU_EFFI,
    FIQ_CPU_PMU_PERF,
    FIQ_VGIC_MI,
    AIC_NR_FIQ,
    AIC_NR_SWIPI,
);

/// One of our one-argument `const fn`s at a probe value. `genmask`, `field_get` and `field_prep` take two arguments and are
/// in `OURS_ONLY`, so they are not here.
///
/// The widening is per arm, not up front: `mpidr_cpu`, `mpidr_cluster` and `timer_firing` take a `u64`, and their MPIDR-shaped
/// probes do not fit a `u32`, so a blanket conversion would report those probes as an unknown function.
fn our_fn(name: &str, arg: u64) -> Option<u64> {
    use lucina_apple_aic::regs as r;
    let w = u32::try_from(arg).ok();
    Some(match name {
        "aic_cpu_ipi_set" => r::aic_cpu_ipi_set(w?) as u64,
        "aic_cpu_ipi_clr" => r::aic_cpu_ipi_clr(w?) as u64,
        "aic_cpu_ipi_mask_set" => r::aic_cpu_ipi_mask_set(w?) as u64,
        "aic_cpu_ipi_mask_clr" => r::aic_cpu_ipi_mask_clr(w?) as u64,
        "mask_reg" => r::mask_reg(w?) as u64,
        "mask_bit" => r::mask_bit(w?) as u64,
        "bits_to_u32" => r::bits_to_u32(w?) as u64,
        "mpidr_cpu" => r::mpidr_cpu(arg),
        "mpidr_cluster" => r::mpidr_cluster(arg),
        "timer_firing" => u64::from(r::timer_firing(arg)),
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// The original: read with `git show`, then its constants out of the text.
// ---------------------------------------------------------------------------

/// The clone to read from, or `None` when `LUCINA_ASAHI_LINUX` is unset: this host has no original, so nothing is compared.
fn clone_dir() -> Option<std::path::PathBuf> {
    let raw = std::env::var(CLONE_VAR).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let path = std::path::PathBuf::from(trimmed);
    assert!(path.is_dir(), "{CLONE_VAR}={path:?} is not a directory");
    assert!(
        path.join(".git").exists(),
        "{CLONE_VAR}={path:?} is not a git clone of the original (no .git in it)"
    );
    Some(path)
}

/// `git show <rev>:<path>` in the clone: the original text of the pinned revision, from the object store. Nothing is
/// written, and nothing of it is kept.
fn original(clone: &std::path::Path, file: Orig) -> String {
    let spec = format!("{ORIGINAL_REV}:{}", file.path());
    let out = Command::new("git")
        .arg("-C")
        .arg(clone)
        .arg("show")
        .arg(&spec)
        .output()
        .expect("`git show` runs: git is on PATH");
    assert!(
        out.status.success(),
        "git -C {} show {spec} failed: {}",
        clone.display(),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Tok {
    Num(u64),
    Id(String),
    P(&'static str),
}

/// The operators the evaluator knows, longest first so `<<` is not read as two `<`.
const OPS: &[&str] = &[
    "<<", ">>", "<=", ">=", "==", "!=", "&&", "||", "+", "-", "*", "/", "%", "&", "|", "^", "~",
    "!", "(", ")", ",", "?", ":", "<", ">", "=", "{", "}", "[", "]", ";", ".",
];

/// The kernel's integer type names and their size in bytes, for `sizeof`: `__u32` is `unsigned int`
/// (`include/uapi/asm-generic/int-ll64.h:27`) and `u32` is `__u32` (`include/linux/types.h:27`), 4 bytes; arm64 Linux is
/// LP64, so `long` is 8. A type not listed here is an error rather than a guess.
fn type_size(name: &str) -> Option<u64> {
    Some(match name {
        "u8" | "__u8" | "s8" | "__s8" | "char" | "unsigned char" => 1,
        "u16" | "__u16" | "s16" | "__s16" | "short" | "unsigned short" => 2,
        "u32" | "__u32" | "s32" | "__s32" | "int" | "unsigned" | "unsigned int" => 4,
        "u64" | "__u64" | "s64" | "__s64" | "long" | "unsigned long" | "size_t" | "long long"
        | "unsigned long long" => 8,
        _ => return None,
    })
}

/// The words of a cast: a cast changes the C type, and every value compared here fits in ours, so the test drops it.
const CAST_WORDS: &[&str] = &[
    "unsigned", "signed", "const", "volatile", "long", "int", "char", "short", "struct",
];

/// The macros the evaluator implements by meaning, not by body (see the module header).
const BUILTINS: &[&str] = &[
    "BIT",
    "BIT_ULL",
    "GENMASK",
    "GENMASK_ULL",
    "GENMASK_U32",
    "UL",
    "ULL",
];

/// The bits `lo..=hi` of a 64-bit word, as the kernel's `GENMASK` computes them.
fn mask_between(hi: u32, lo: u32) -> u64 {
    let high = if hi >= 63 {
        u64::MAX
    } else {
        (1u64 << (hi + 1)) - 1
    };
    let low = if lo == 0 { 0 } else { (1u64 << lo) - 1 };
    high & !low
}

fn tokenize(text: &str) -> Result<Vec<Tok>, String> {
    let c: Vec<char> = text.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    while i < c.len() {
        let ch = c[i];
        if ch.is_whitespace() {
            i += 1;
            continue;
        }
        if ch.is_ascii_alphabetic() || ch == '_' {
            let s = i;
            while i < c.len() && (c[i].is_ascii_alphanumeric() || c[i] == '_') {
                i += 1;
            }
            out.push(Tok::Id(c[s..i].iter().collect()));
            continue;
        }
        if ch.is_ascii_digit() {
            let s = i;
            let hex = ch == '0' && matches!(c.get(i + 1), Some('x') | Some('X'));
            if hex {
                i += 2;
                while i < c.len() && (c[i].is_ascii_hexdigit() || c[i] == '_') {
                    i += 1;
                }
            } else {
                while i < c.len() && (c[i].is_ascii_digit() || c[i] == '_') {
                    i += 1;
                }
                if ch == '0' && i - s > 1 {
                    let lit: String = c[s..i].iter().collect();
                    return Err(format!(
                        "a leading-zero literal (`{lit}`) is octal and is not handled"
                    ));
                }
            }
            let digits: String = c[s + if hex { 2 } else { 0 }..i]
                .iter()
                .filter(|x| **x != '_')
                .collect();
            while i < c.len() && matches!(c[i], 'u' | 'U' | 'l' | 'L') {
                i += 1;
            }
            if i < c.len() && (c[i].is_ascii_alphanumeric() || c[i] == '_') {
                return Err(format!("bad numeric literal in `{text}`"));
            }
            let value = if hex {
                u64::from_str_radix(&digits, 16)
            } else {
                digits.parse::<u64>()
            }
            .map_err(|e| format!("literal `{digits}`: {e}"))?;
            out.push(Tok::Num(value));
            continue;
        }
        match OPS.iter().find(|op| text[i..].starts_with(**op)) {
            Some(op) => {
                out.push(Tok::P(op));
                i += op.chars().count();
            }
            None => {
                return Err(format!(
                    "`{ch}` is not an operator this evaluator knows (in `{text}`)"
                ));
            }
        }
    }
    Ok(out)
}

#[derive(Clone, Debug)]
enum Sym {
    /// `#define NAME body` (`params` is `None`) or `#define NAME(a, b) body`. `text` is the line, for the messages.
    Macro {
        file: Orig,
        line: usize,
        text: String,
        params: Option<Vec<String>>,
        body: Vec<Tok>,
    },
    /// An enum member: `init` is its `= ...` if it has one, otherwise it is `prev` plus one.
    Enum {
        file: Orig,
        line: usize,
        text: String,
        init: Option<Vec<Tok>>,
        prev: Option<String>,
    },
    /// A `#define` whose body this evaluator cannot even lex: only a problem if something reaches it.
    Unreadable {
        file: Orig,
        line: usize,
        text: String,
        why: String,
    },
}

impl Sym {
    fn site(&self) -> String {
        let (file, line, text) = match self {
            Sym::Macro {
                file, line, text, ..
            }
            | Sym::Enum {
                file, line, text, ..
            } => (*file, *line, text.clone()),
            Sym::Unreadable {
                file, line, text, ..
            } => (*file, *line, text.clone()),
        };
        format!("{}:{} `{text}`", file.path(), line)
    }

    fn file(&self) -> Orig {
        match self {
            Sym::Macro { file, .. } | Sym::Enum { file, .. } | Sym::Unreadable { file, .. } => {
                *file
            }
        }
    }
}

/// The constants of the original files at the pinned revision: every `#define` and every enum member, with each name's value
/// worked out lazily, so a body the evaluator cannot handle only matters when something reaches it.
struct Model {
    syms: BTreeMap<(Orig, String), Sym>,
    /// A bare name to the files that define it, in `Orig::PARSED` order.
    homes: BTreeMap<String, Vec<Orig>>,
    lines: BTreeMap<Orig, Vec<String>>,
    cache: std::cell::RefCell<BTreeMap<String, Result<u64, String>>>,
}

/// `#define` lines joined across `\<newline>` continuations: `(the line it starts on, its text)`.
fn logical_lines(lines: &[String]) -> Vec<(usize, String)> {
    let mut out: Vec<(usize, String)> = Vec::new();
    let mut start = 0;
    let mut acc = String::new();
    let mut joined = false;
    for (i, raw) in lines.iter().enumerate() {
        let piece = raw.trim_end();
        let (content, cont) = match piece.strip_suffix('\\') {
            Some(head) => (head.trim_end(), true),
            None => (piece.trim_start(), false),
        };
        if !joined {
            start = i + 1;
            acc.clear();
            joined = true;
        }
        acc.push_str(content);
        if cont {
            acc.push(' ');
        } else {
            out.push((start, std::mem::take(&mut acc)));
            joined = false;
        }
    }
    if joined && !acc.trim().is_empty() {
        out.push((start, acc));
    }
    out
}

/// Blanks out comments, keeping every line where it is, so a comment cannot be read as code and the line numbers stay true.
/// String and character literals are copied through untouched.
fn strip_comments(text: &str) -> String {
    let c: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let (mut i, mut block, mut line, mut str_, mut chr) = (0usize, false, false, false, false);
    while i < c.len() {
        let ch = c[i];
        let next = c.get(i + 1).copied();
        if block {
            if ch == '*' && next == Some('/') {
                block = false;
                i += 2;
                out.push_str("  ");
                continue;
            }
            out.push(if ch == '\n' { '\n' } else { ' ' });
            i += 1;
            continue;
        }
        if line {
            if ch == '\n' {
                line = false;
                out.push('\n');
            } else {
                out.push(' ');
            }
            i += 1;
            continue;
        }
        if str_ || chr {
            out.push(ch);
            if ch == '\\'
                && let Some(n) = next
            {
                out.push(n);
                i += 2;
                continue;
            }
            if (str_ && ch == '"') || (chr && ch == '\'') {
                str_ = false;
                chr = false;
            }
            i += 1;
            continue;
        }
        if ch == '/' && next == Some('*') {
            block = true;
            i += 2;
            out.push_str("  ");
            continue;
        }
        if ch == '/' && next == Some('/') {
            line = true;
            i += 2;
            out.push_str("  ");
            continue;
        }
        if ch == '"' {
            str_ = true;
        } else if ch == '\'' {
            chr = true;
        }
        out.push(ch);
        i += 1;
    }
    out
}

impl Model {
    fn load(clone: &std::path::Path) -> Model {
        let mut m = Model {
            syms: BTreeMap::new(),
            homes: BTreeMap::new(),
            lines: BTreeMap::new(),
            cache: std::cell::RefCell::new(BTreeMap::new()),
        };
        for file in [Orig::PARSED, &[Orig::Sysreg]].concat() {
            let text = match file {
                // The sysreg table is a list of rows, not C: read raw, it is only scanned for rows.
                Orig::Sysreg => original(clone, file),
                _ => strip_comments(&original(clone, file)),
            };
            m.lines
                .insert(file, text.lines().map(str::to_string).collect());
        }
        for file in Orig::PARSED {
            let lines = m.lines[file].clone();
            m.parse_defines(*file, &lines);
            m.parse_enums(*file, &lines);
        }
        m
    }

    fn parse_defines(&mut self, file: Orig, lines: &[String]) {
        for (no, text) in logical_lines(lines) {
            let t = text.trim_start();
            let Some(at) = t.strip_prefix('#').map(str::trim_start) else {
                continue;
            };
            if let Some(d) = at.strip_prefix("define").map(str::trim_start) {
                let end = d
                    .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .unwrap_or(d.len());
                let name = &d[..end];
                if name.is_empty() {
                    continue;
                }
                // Only a `(` glued to the name makes it function-like: `#define F (x)` is an object with a body.
                let tail = &d[end..];
                let (params, body) = match tail.strip_prefix('(') {
                    Some(open) => {
                        let close = open.find(')').unwrap_or(open.len());
                        let ps = open[..close]
                            .split(',')
                            .map(str::trim)
                            .filter(|p| !p.is_empty())
                            .map(str::to_string)
                            .collect();
                        (Some(ps), open[close + 1..].trim().to_string())
                    }
                    None => (None, tail.trim().to_string()),
                };
                let key = (file, name.to_string());
                if self.syms.contains_key(&key) {
                    continue; // the first definition wins, which is the C branch of the two headers that have both
                }
                let sym = match tokenize(&body) {
                    Ok(body) => Sym::Macro {
                        file,
                        line: no,
                        text: text.trim().to_string(),
                        params,
                        body,
                    },
                    Err(why) => Sym::Unreadable {
                        file,
                        line: no,
                        text: text.trim().to_string(),
                        why,
                    },
                };
                self.syms.insert(key, sym);
                self.homes.entry(name.to_string()).or_default().push(file);
            } else if let Some(d) = at.strip_prefix("undef").map(str::trim) {
                let name = d.to_string();
                self.syms.remove(&(file, name.clone()));
                if let Some(at) = self.homes.get_mut(&name) {
                    at.retain(|f| *f != file);
                }
            }
        }
    }

    /// The members of `enum { ... }` and `enum name { ... }`, each with the member before it so a value written as nothing
    /// (`AIC_NR_FIQ`, with no `= ...`) can be worked out as that one plus one. A header whose `{` is not on its own line is
    /// skipped, which only means those members are unknown.
    fn parse_enums(&mut self, file: Orig, lines: &[String]) {
        let mut n = 0;
        while n < lines.len() {
            let t = lines[n].trim();
            let head = t.split_whitespace().next().unwrap_or("");
            if head != "enum" || !t.contains('{') {
                n += 1;
                continue;
            }
            let mut depth = 0usize;
            let mut entry = String::new();
            let mut entry_line = n + 1;
            let mut prev: Option<String> = None;
            let mut done = false;
            for (li, raw) in lines.iter().enumerate().skip(n) {
                for ch in raw.chars() {
                    if ch == '{' {
                        depth += 1;
                        continue;
                    }
                    if ch == '}' {
                        depth -= 1;
                        if depth == 0 {
                            done = true;
                            break;
                        }
                    }
                    if depth == 0 {
                        continue;
                    }
                    if ch == ',' && depth == 1 {
                        self.enum_entry(file, entry_line, &entry, &mut prev);
                        entry.clear();
                        entry_line = li + 1;
                        continue;
                    }
                    if entry.trim().is_empty() && !ch.is_whitespace() {
                        entry_line = li + 1;
                    }
                    entry.push(if ch == '\n' { ' ' } else { ch });
                }
                entry.push(' ');
                if done {
                    break;
                }
            }
            if !entry.trim().is_empty() {
                self.enum_entry(file, entry_line, &entry, &mut prev);
            }
            n += 1;
            while n < lines.len() && !lines[n].contains('}') {
                n += 1;
            }
            n += 1;
        }
    }

    fn enum_entry(&mut self, file: Orig, line: usize, raw: &str, prev: &mut Option<String>) {
        let e = raw.trim();
        if e.is_empty() {
            return;
        }
        let (name, init) = match e.split_once('=') {
            Some((a, b)) => (a.trim().to_string(), Some(b.trim().to_string())),
            None => (e.to_string(), None),
        };
        let valid = !name.is_empty()
            && !name.starts_with(|c: char| c.is_ascii_digit())
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !valid {
            return;
        }
        let key = (file, name.clone());
        if !self.syms.contains_key(&key) {
            let toks = match init.as_deref() {
                Some(text) => match tokenize(text) {
                    Ok(v) => Some(v),
                    Err(why) => {
                        self.syms.insert(
                            key.clone(),
                            Sym::Unreadable {
                                file,
                                line,
                                text: e.to_string(),
                                why,
                            },
                        );
                        self.homes.entry(name.clone()).or_default().push(file);
                        *prev = Some(name);
                        return;
                    }
                },
                None => None,
            };
            self.syms.insert(
                key,
                Sym::Enum {
                    file,
                    line,
                    text: e.to_string(),
                    init: toks,
                    prev: prev.clone(),
                },
            );
            self.homes.entry(name.clone()).or_default().push(file);
        }
        *prev = Some(name);
    }

    fn lookup(&self, name: &str, home: Orig) -> Option<&Sym> {
        if let Some(s) = self.syms.get(&(home, name.to_string())) {
            return Some(s);
        }
        for f in self.homes.get(name).into_iter().flatten() {
            if let Some(s) = self.syms.get(&(*f, name.to_string())) {
                return Some(s);
            }
        }
        None
    }

    /// How the original writes `NAME` of `file`, for the messages.
    fn describe(&self, name: &str, home: Orig) -> String {
        match self.lookup(name, home) {
            Some(s) => s.site(),
            None => format!("`{name}`, which none of the files read defines"),
        }
    }

    /// `NAME` in `file`, as the C preprocessor and compiler would see it.
    fn value(&self, name: &str, home: Orig) -> Result<u64, String> {
        let key = format!("{}:{}", home.path(), name);
        if let Some(hit) = self.cache.borrow().get(&key) {
            return hit.clone();
        }
        let mut stack = Vec::new();
        let r = self.compute(name, home, &mut Vec::new(), &mut stack);
        self.cache.borrow_mut().insert(key, r.clone());
        r
    }

    /// A function-like `NAME(params)` with these parameters bound: the probe of a macro.
    fn probe(&self, name: &str, home: Orig, args: &[(String, Vec<Tok>)]) -> Result<u64, String> {
        let sym = self
            .lookup(name, home)
            .ok_or_else(|| format!("`{name}` is not defined in the files this test reads"))?;
        let Sym::Macro { params, body, .. } = sym else {
            return Err(format!(
                "`{name}` at {} is not a function-like macro",
                sym.site()
            ));
        };
        let want = params.as_ref().map_or(0, |p| p.len());
        if want != args.len() {
            return Err(format!(
                "`{name}` at {} takes {want} parameters; the test binds {}",
                sym.site(),
                args.len()
            ));
        }
        let mut env: Vec<(String, Vec<Tok>)> = args.to_vec();
        let mut stack = vec![format!("{name} at {}", sym.site())];
        self.eval_tokens(body, sym.file(), &mut env, &mut stack)
    }

    /// The one parameter of a function-like macro that the test does not hold fixed: it is what the probe values bind to.
    fn free_param(&self, name: &str, home: Orig, fixed: &[&str]) -> Result<String, String> {
        let sym = self
            .lookup(name, home)
            .ok_or_else(|| format!("`{name}` is not defined in the files this test reads"))?;
        let Sym::Macro { params, .. } = sym else {
            return Err(format!(
                "`{name}` at {} is not a function-like macro",
                sym.site()
            ));
        };
        let ps = params
            .as_ref()
            .ok_or_else(|| format!("`{name}` at {} takes no parameters", sym.site()))?;
        let free: Vec<&String> = ps.iter().filter(|p| !fixed.contains(&p.as_str())).collect();
        match free.as_slice() {
            [one] => Ok((*one).clone()),
            _ => Err(format!(
                "`{name}` at {} has parameters {}; the test holds {} fixed and probes exactly one",
                sym.site(),
                ps.join(", "),
                fixed.join(", ")
            )),
        }
    }

    fn compute(
        &self,
        name: &str,
        home: Orig,
        env: &mut Vec<(String, Vec<Tok>)>,
        stack: &mut Vec<String>,
    ) -> Result<u64, String> {
        let trail = format!("{name} at {}", self.describe(name, home));
        if stack.contains(&trail) {
            let mut all = stack.clone();
            all.push(trail.clone());
            return Err(format!("circular reference: {}", all.join(" -> ")));
        }
        let sym = match self.lookup(name, home) {
            Some(s) => s,
            None => {
                return Err(format!(
                    "`{name}` is not defined in the files this test reads"
                ));
            }
        };
        stack.push(trail.clone());
        let r = match sym.clone() {
            Sym::Unreadable { why, .. } => Err(format!(
                "its body is not something this evaluator can lex: {why}"
            )),
            Sym::Enum { init, prev, .. } => match (init, prev) {
                (Some(t), _) => self.eval_tokens(&t, sym.file(), env, stack),
                (None, Some(p)) => self.compute(&p, sym.file(), env, stack).map(|v| v + 1),
                (None, None) => Ok(0),
            },
            Sym::Macro {
                params: Some(ps),
                body,
                ..
            } => {
                let bound: Vec<(String, Vec<Tok>)> = ps
                    .iter()
                    .map(|p| {
                        let arg = env
                            .iter()
                            .rev()
                            .find(|(k, _)| k == p)
                            .map(|(_, a)| a.clone())
                            .ok_or_else(|| format!("`{p}` of `{name}` is not bound"));
                        arg.map(|a| (p.clone(), a))
                    })
                    .collect::<Result<_, _>>()?;
                let saved = env.len();
                env.extend(bound);
                let v = self.eval_tokens(&body, sym.file(), env, stack);
                env.truncate(saved);
                v
            }
            Sym::Macro {
                params: None, body, ..
            } => self.eval_tokens(&body, sym.file(), env, stack),
        };
        stack.pop();
        r.map_err(|e| {
            if e.contains(name) {
                e
            } else {
                format!("{e} (in {trail})")
            }
        })
    }

    fn eval_tokens(
        &self,
        toks: &[Tok],
        home: Orig,
        env: &mut Vec<(String, Vec<Tok>)>,
        stack: &mut Vec<String>,
    ) -> Result<u64, String> {
        let as_text = toks.iter().map(tok_text).collect::<Vec<_>>().join("");
        let expanded = self
            .expand(toks, home, env, 0)
            .map_err(|e| format!("{e} (in `{as_text}`)"))?;
        let mut p = Parser {
            m: self,
            t: expanded,
            i: 0,
            home,
            env,
            stack,
        };
        let v = p.expr().map_err(|e| format!("{e} (in `{as_text}`)"))?;
        if p.i != p.t.len() {
            return Err(format!("`{as_text}` has tokens left over"));
        }
        Ok(v)
    }

    /// The preprocessor pass: a bound parameter or a macro name is replaced by its tokens; a built-in keeps its call, and
    /// its arguments are expanded when the evaluator reaches them.
    fn expand(
        &self,
        toks: &[Tok],
        home: Orig,
        env: &mut Vec<(String, Vec<Tok>)>,
        depth: usize,
    ) -> Result<Vec<Tok>, String> {
        if depth > 32 {
            return Err("macro expansion is deeper than 32 levels".to_string());
        }
        let mut out: Vec<Tok> = Vec::new();
        let mut i = 0;
        while i < toks.len() {
            let Tok::Id(name) = &toks[i] else {
                out.push(toks[i].clone());
                i += 1;
                continue;
            };
            let bound = env
                .iter()
                .rev()
                .find(|(k, _)| k == name)
                .map(|(_, a)| a.clone());
            if let Some(arg) = bound {
                out.extend(self.expand(&arg, home, env, depth + 1)?);
                i += 1;
                continue;
            }
            if BUILTINS.contains(&name.as_str()) {
                out.push(Tok::Id(name.clone()));
                i += 1;
                if i >= toks.len() || toks[i] != Tok::P("(") {
                    return Err(format!("`{name}` needs an argument list"));
                }
                let open = i;
                let mut d = 0i32;
                let mut end = None;
                for (j, t) in toks.iter().enumerate().skip(open) {
                    match t {
                        Tok::P("(") => d += 1,
                        Tok::P(")") => d -= 1,
                        _ => {}
                    }
                    if d == 0 {
                        end = Some(j);
                        break;
                    }
                }
                let end =
                    end.ok_or_else(|| format!("`{name}` is missing its closing parenthesis"))?;
                out.extend(toks[open..=end].iter().cloned());
                i = end + 1;
                continue;
            }
            match self.lookup(name, home) {
                Some(Sym::Macro {
                    params: Some(ps),
                    body,
                    ..
                }) => {
                    let mut j = i + 1;
                    if j >= toks.len() || toks[j] != Tok::P("(") {
                        return Err(format!("`{name}` is used without its argument list"));
                    }
                    j += 1;
                    let mut d = 1usize;
                    let mut args: Vec<Vec<Tok>> = vec![Vec::new()];
                    while j < toks.len() && d > 0 {
                        match &toks[j] {
                            Tok::P("(") => {
                                d += 1;
                                args.last_mut().unwrap().push(toks[j].clone());
                            }
                            Tok::P(")") => {
                                d -= 1;
                                if d > 0 {
                                    args.last_mut().unwrap().push(toks[j].clone());
                                }
                            }
                            Tok::P(",") if d == 1 => args.push(Vec::new()),
                            t => args.last_mut().unwrap().push(t.clone()),
                        }
                        j += 1;
                    }
                    if d != 0 {
                        return Err(format!("`{name}` is missing its closing parenthesis"));
                    }
                    if args.len() != ps.len() {
                        return Err(format!(
                            "`{name}` takes {} parameters, the call gives {}",
                            ps.len(),
                            args.len()
                        ));
                    }
                    let base = env.len();
                    for (p, a) in ps.iter().zip(args) {
                        // C expands an argument before substituting it, and here that is also what keeps the walk finite:
                        // `MPIDR_AFFINITY_LEVEL` calls `MPIDR_LEVEL_SHIFT(level)`, whose own parameter is named `level`, so
                        // binding the raw token would make `level` stand for `level` and expand it forever.
                        let a = self.expand(&a, home, env, depth + 1)?;
                        env.push((p.clone(), a));
                    }
                    let expanded = self.expand(body, home, env, depth + 1);
                    env.truncate(base);
                    out.extend(expanded?);
                    i = j;
                }
                Some(Sym::Macro {
                    params: None, body, ..
                }) => {
                    let expanded = self.expand(body, home, env, depth + 1)?;
                    out.extend(expanded);
                    i += 1;
                }
                // An enum member, a type name, a cast word: the evaluator reaches it.
                _ => {
                    out.push(Tok::Id(name.clone()));
                    i += 1;
                }
            }
        }
        Ok(out)
    }
}

fn tok_text(t: &Tok) -> String {
    match t {
        Tok::Num(v) => format!("{v}"),
        Tok::Id(s) => s.clone(),
        Tok::P(p) => (*p).to_string(),
    }
}

/// The mask of `Field <bit> <name>` or `Field <hi>:<lo> <name>` inside a `Sysreg <reg>` block of the sysreg table.
fn sysreg_field(lines: &[String], reg: &str, field: &str) -> Result<(u64, usize), String> {
    let mut in_block = false;
    for (i, raw) in lines.iter().enumerate() {
        let parts: Vec<&str> = raw.split_whitespace().collect();
        match parts.first().copied() {
            Some("Sysreg") | Some("Sysreg_12bit") | Some("Sysreg_16bit") => {
                in_block = parts.get(1) == Some(&reg)
            }
            Some("EndSysreg") => in_block = false,
            Some("Field") if in_block && parts.len() == 3 && parts[2] == field => {
                let (hi, lo) = match parts[1].split_once(':') {
                    Some((a, b)) => (
                        a.parse::<u32>().map_err(|e| format!("`{a}`: {e}"))?,
                        b.parse::<u32>().map_err(|e| format!("`{b}`: {e}"))?,
                    ),
                    None => {
                        let b = parts[1]
                            .parse::<u32>()
                            .map_err(|e| format!("`{}`: {e}", parts[1]))?;
                        (b, b)
                    }
                };
                return Ok((mask_between(hi, lo), i + 1));
            }
            _ => {}
        }
    }
    Err(format!(
        "no row `Field ... {field}` in the `Sysreg {reg}` block"
    ))
}

// ---------------------------------------------------------------------------
// The evaluator.
// ---------------------------------------------------------------------------

/// Recursive descent over the expanded tokens: C's precedence, `u64` arithmetic.
struct Parser<'a> {
    m: &'a Model,
    t: Vec<Tok>,
    i: usize,
    home: Orig,
    env: &'a mut Vec<(String, Vec<Tok>)>,
    stack: &'a mut Vec<String>,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Tok> {
        self.t.get(self.i)
    }

    fn at(&self, op: &str) -> bool {
        matches!(self.peek(), Some(Tok::P(p)) if *p == op)
    }

    fn eat(&mut self, op: &str) -> bool {
        if self.at(op) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn level(
        &mut self,
        ops: &[&str],
        lower: fn(&mut Self) -> Result<u64, String>,
    ) -> Result<u64, String> {
        let mut v = lower(self)?;
        while let Some(Tok::P(p)) = self.peek().cloned() {
            if !ops.contains(&p) {
                break;
            }
            self.i += 1;
            let rhs = lower(self)?;
            v = match p {
                "||" => u64::from(v != 0 || rhs != 0),
                "&&" => u64::from(v != 0 && rhs != 0),
                "|" => v | rhs,
                "^" => v ^ rhs,
                "&" => v & rhs,
                "==" => u64::from(v == rhs),
                "!=" => u64::from(v != rhs),
                "<" => u64::from(v < rhs),
                ">" => u64::from(v > rhs),
                "<=" => u64::from(v <= rhs),
                ">=" => u64::from(v >= rhs),
                "<<" => v
                    .checked_shl(u32::try_from(rhs).unwrap_or(u32::MAX))
                    .unwrap_or(0),
                ">>" => v
                    .checked_shr(u32::try_from(rhs).unwrap_or(u32::MAX))
                    .unwrap_or(0),
                "+" => v.wrapping_add(rhs),
                "-" => v.wrapping_sub(rhs),
                "*" => v.wrapping_mul(rhs),
                "/" => {
                    if rhs == 0 {
                        return Err("division by zero in a kernel constant".to_string());
                    }
                    v / rhs
                }
                "%" => {
                    if rhs == 0 {
                        return Err("remainder by zero in a kernel constant".to_string());
                    }
                    v % rhs
                }
                other => return Err(format!("operator `{other}` is not handled")),
            };
        }
        Ok(v)
    }

    fn expr(&mut self) -> Result<u64, String> {
        let c = self.level(&["||"], Self::l_and)?;
        if self.eat("?") {
            let t = self.expr()?;
            if !self.eat(":") {
                return Err("`?` with no `:`".to_string());
            }
            let f = self.expr()?;
            return Ok(if c != 0 { t } else { f });
        }
        Ok(c)
    }

    fn l_and(&mut self) -> Result<u64, String> {
        self.level(&["&&"], Self::bit_or)
    }

    fn bit_or(&mut self) -> Result<u64, String> {
        self.level(&["|"], Self::bit_xor)
    }

    fn bit_xor(&mut self) -> Result<u64, String> {
        self.level(&["^"], Self::bit_and)
    }

    fn bit_and(&mut self) -> Result<u64, String> {
        self.level(&["&"], Self::equality)
    }

    fn equality(&mut self) -> Result<u64, String> {
        self.level(&["==", "!="], Self::relation)
    }

    fn relation(&mut self) -> Result<u64, String> {
        self.level(&["<", ">", "<=", ">="], Self::shift)
    }

    fn shift(&mut self) -> Result<u64, String> {
        self.level(&["<<", ">>"], Self::add)
    }

    fn add(&mut self) -> Result<u64, String> {
        self.level(&["+", "-"], Self::mul)
    }

    fn mul(&mut self) -> Result<u64, String> {
        self.level(&["*", "/", "%"], Self::unary)
    }

    fn unary(&mut self) -> Result<u64, String> {
        if self.eat("-") {
            return Ok(self.unary()?.wrapping_neg());
        }
        if self.eat("~") {
            return Ok(!self.unary()?);
        }
        if self.eat("!") {
            return Ok(u64::from(self.unary()? == 0));
        }
        if self.eat("+") {
            return self.unary();
        }
        if self.at("(")
            && let Some(n) = self.cast_len()
        {
            self.i += n;
            return self.unary();
        }
        self.primary()
    }

    /// `(` cast-words `)`: the length of a cast, or none if this `(` opens a parenthesised expression. The width never
    /// matters here (see the module header), so a cast is dropped.
    fn cast_len(&self) -> Option<usize> {
        let mut k = self.i + 1;
        let mut words = 0;
        while let Some(Tok::Id(w)) = self.t.get(k) {
            if !CAST_WORDS.contains(&w.as_str()) && type_size(w).is_none() {
                return None;
            }
            words += 1;
            k += 1;
        }
        if words == 0 || k >= self.t.len() || self.t[k] != Tok::P(")") {
            return None;
        }
        Some(k + 1 - self.i)
    }

    fn call_args(&mut self) -> Result<Vec<u64>, String> {
        if !self.eat("(") {
            return Err("expected `(` in a built-in call".to_string());
        }
        let mut out = Vec::new();
        if self.eat(")") {
            return Ok(out);
        }
        loop {
            let start = self.i;
            let mut depth = 0i32;
            while let Some(t) = self.t.get(self.i) {
                match t {
                    Tok::P("(") => depth += 1,
                    Tok::P(")") if depth == 0 => break,
                    Tok::P(")") => depth -= 1,
                    Tok::P(",") if depth == 0 => break,
                    _ => {}
                }
                self.i += 1;
            }
            let slice: Vec<Tok> = self.t[start..self.i].to_vec();
            out.push(
                self.m
                    .eval_tokens(&slice, self.home, self.env, self.stack)?,
            );
            if self.eat(",") {
                continue;
            }
            if self.eat(")") {
                return Ok(out);
            }
            return Err("expected `,` or `)` in an argument list".to_string());
        }
    }

    /// A built-in call, with its arguments expanded and evaluated first.
    fn builtin(&mut self, name: &str) -> Result<u64, String> {
        let args = self.call_args()?;
        match (name, args.as_slice()) {
            ("BIT", [n]) | ("BIT_ULL", [n]) => Ok(1u64
                .checked_shl(u32::try_from(*n).unwrap_or(u32::MAX))
                .unwrap_or(0)),
            ("UL", [v]) | ("ULL", [v]) => Ok(*v),
            ("GENMASK", [h, l]) | ("GENMASK_ULL", [h, l]) | ("GENMASK_U32", [h, l]) => {
                if l > h {
                    return Err(format!(
                        "GENMASK({h}, {l}) has its arguments the wrong way round"
                    ));
                }
                Ok(mask_between(*h as u32, *l as u32))
            }
            (b, a) => Err(format!(
                "built-in `{b}` with {} argument(s) is not handled",
                a.len()
            )),
        }
    }

    fn primary(&mut self) -> Result<u64, String> {
        match self.peek().cloned() {
            Some(Tok::Num(v)) => {
                self.i += 1;
                Ok(v)
            }
            Some(Tok::P("(")) => {
                self.i += 1;
                let v = self.expr()?;
                if !self.eat(")") {
                    return Err("unbalanced `(`".to_string());
                }
                Ok(v)
            }
            Some(Tok::Id(name)) => {
                self.i += 1;
                if BUILTINS.contains(&name.as_str()) {
                    return self.builtin(&name);
                }
                if name == "sizeof" {
                    return self.sizeof();
                }
                self.m.compute(&name, self.home, self.env, self.stack)
            }
            Some(Tok::P(op)) => Err(format!("unexpected `{op}`")),
            None => Err("the expression ends too early".to_string()),
        }
    }

    fn sizeof(&mut self) -> Result<u64, String> {
        let mut words: Vec<String> = Vec::new();
        if self.eat("(") {
            while let Some(Tok::Id(w)) = self.peek().cloned() {
                words.push(w);
                self.i += 1;
            }
            if !self.eat(")") {
                return Err("sizeof of something that is not a plain type name".to_string());
            }
        } else if let Some(Tok::Id(w)) = self.peek().cloned() {
            words.push(w);
            self.i += 1;
        }
        let name = words.join(" ");
        type_size(&name).ok_or_else(|| format!("sizeof({name}) is not a type this evaluator knows"))
    }
}

// ---------------------------------------------------------------------------
// The tests.
// ---------------------------------------------------------------------------

/// Every constant of ours is in one of the tables, each table entry names something that is really in the crate, and nothing
/// is in two tables. This one runs without the clone: it needs only this repository. A new `pub const` in `src/regs.rs`
/// lands here until it is classified.
#[test]
fn every_constant_is_classified() {
    let declared = ours_declared();
    fn claim(
        tables: &mut BTreeMap<&'static str, &'static str>,
        name: &'static str,
        table: &'static str,
    ) {
        if tables.insert(name, table).is_some() {
            panic!("`{name}` is in two tables");
        }
    }
    let mut tables: BTreeMap<&'static str, &'static str> = BTreeMap::new();
    for (ours, _, _) in SCALARS {
        claim(&mut tables, ours, "SCALARS");
    }
    for (ours, _, _, _, _) in MACROS {
        claim(&mut tables, ours, "MACROS");
    }
    for (ours, _) in NOT_A_DEFINE {
        claim(&mut tables, ours, "NOT_A_DEFINE");
    }
    for (ours, _) in OURS_ONLY {
        claim(&mut tables, ours, "OURS_ONLY");
    }
    let mut unclassified: Vec<String> = Vec::new();
    for (name, (line, kind)) in &declared {
        match tables.get(name.as_str()) {
            Some(table) => {
                let fits = matches!(
                    (kind, *table),
                    (Ours::Scalar, "SCALARS")
                        | (Ours::Scalar, "NOT_A_DEFINE")
                        | (Ours::Func, "MACROS")
                        | (Ours::Func, "OURS_ONLY")
                );
                assert!(
                    fits,
                    "`{name}` ({OURS_PATH}:{line}) is {kind:?} but is filed under {table}"
                );
            }
            None => unclassified.push(format!("{OURS_PATH}:{line} {name}")),
        }
    }
    for (name, table) in &tables {
        assert!(
            declared.contains_key(*name),
            "the table {table} names `{name}`, which src/regs.rs does not declare any more"
        );
    }
    assert!(
        unclassified.is_empty(),
        "{} of our constants are in no table, so nothing compares them; put each in SCALARS, MACROS, NOT_A_DEFINE or \
         OURS_ONLY:\n  {}",
        unclassified.len(),
        unclassified.join("\n  ")
    );
    assert_eq!(
        declared
            .values()
            .filter(|(_, k)| *k == Ours::Scalar)
            .count(),
        SCALARS.len() + NOT_A_DEFINE.len(),
        "SCALARS and NOT_A_DEFINE do not cover every scalar of ours"
    );
    assert_eq!(
        declared.values().filter(|(_, k)| *k == Ours::Func).count(),
        MACROS.len() + OURS_ONLY.len(),
        "MACROS and OURS_ONLY do not cover every const fn of ours"
    );
    eprintln!(
        "classified {} of our items: {} compared by value, {} compared as macro probes, {} whose original is not a \
         #define, {} ours with no counterpart",
        declared.len(),
        SCALARS.len(),
        MACROS.len(),
        NOT_A_DEFINE.len(),
        OURS_ONLY.len()
    );
}

/// The comparison. Every mismatch is collected, so one run names all of them.
#[test]
fn our_constants_equal_the_original() {
    let Some(clone) = clone_dir() else {
        eprintln!("skipped: LUCINA_ASAHI_LINUX not set");
        return;
    };
    let m = Model::load(&clone);
    let declared = ours_declared();
    let mut bad: Vec<String> = Vec::new();
    let mut compared = 0usize;
    let mut probes = 0usize;

    for (ours, file, orig) in SCALARS {
        let mine = our_value(ours)
            .unwrap_or_else(|| panic!("`{ours}` is in SCALARS but not in our_scalar_values!"));
        let ours_at = format!("{OURS_PATH}:{}", declared[*ours].0);
        match m.value(orig, *file) {
            Ok(theirs) => {
                compared += 1;
                if theirs != mine {
                    bad.push(format!(
                        "{ours}: ours {ours_at} = {mine:#x} ({mine}); the original's `{orig}` at {} = {theirs:#x} \
                         ({theirs})",
                        m.describe(orig, *file)
                    ));
                }
            }
            Err(e) => bad.push(format!(
                "{ours}: ours {ours_at} = {mine:#x}; the original's `{orig}` could not be read from {} ({}) because {e}",
                file.path(),
                m.describe(orig, *file)
            )),
        }
    }

    for (ours, file, orig, fixed, at) in MACROS {
        let ours_at = format!("{OURS_PATH}:{}", declared[*ours].0);
        let mut bound: Vec<(String, Vec<Tok>)> = fixed
            .iter()
            .map(|(p, v)| (p.to_string(), vec![Tok::Num(*v)]))
            .collect();
        let free = match m.free_param(
            orig,
            *file,
            &fixed.iter().map(|(p, _)| *p).collect::<Vec<&str>>(),
        ) {
            Ok(f) => f,
            Err(e) => {
                bad.push(format!("{ours}: {e}"));
                continue;
            }
        };
        for v in *at {
            bound.retain(|(p, _)| *p != free);
            bound.push((free.clone(), vec![Tok::Num(*v)]));
            match m.probe(orig, *file, &bound) {
                Ok(theirs) => {
                    probes += 1;
                    let mine = our_fn(ours, *v)
                        .unwrap_or_else(|| panic!("`{ours}` is in MACROS but our_fn does not know it, or its probe {v}"));
                    if theirs != mine {
                        bad.push(format!(
                            "{ours}({v:#x}): ours {ours_at} = {mine:#x} ({mine}); the original's `{orig}({free})` at {} \
                             with {free} = {v:#x} is {theirs:#x} ({theirs})",
                            m.describe(orig, *file)
                        ));
                    }
                }
                Err(e) => bad.push(format!(
                    "{ours}({v:#x}): the original's `{orig}({free})` at {} could not be read because {e}",
                    m.describe(orig, *file)
                )),
            }
        }
    }

    for (ours, what) in NOT_A_DEFINE {
        let mine = our_value(ours)
            .unwrap_or_else(|| panic!("`{ours}` is in NOT_A_DEFINE but not in our_scalar_values!"));
        let ours_at = format!("{OURS_PATH}:{}", declared[*ours].0);
        let NotDefine::SysregField { reg, field, how } = *what;
        let lines = &m.lines[&Orig::Sysreg];
        match sysreg_field(lines, reg, field) {
            Ok((theirs, line)) => {
                compared += 1;
                if theirs != mine {
                    bad.push(format!(
                        "{ours}: ours {ours_at} = {mine:#x} ({mine}); the original's `{field}` field of `{reg}` at \
                         {}:{line} is {theirs:#x} ({theirs}). {how}",
                        Orig::Sysreg.path()
                    ));
                }
            }
            Err(e) => bad.push(format!(
                "{ours}: ours {ours_at} = {mine:#x}; {e} in {} (read at {ORIGINAL_REV}). {how}",
                Orig::Sysreg.path()
            )),
        }
    }

    assert!(
        bad.is_empty(),
        "\n{} of the comparisons against the original at {ORIGINAL_REV} did not hold:\n\n  {}\n",
        bad.len(),
        bad.join("\n\n  ")
    );
    eprintln!(
        "compared {compared} constants and {probes} macro probe points against {ORIGINAL_REV}, reading {} files of the \
         original; {} of ours have no counterpart and are listed in OURS_ONLY",
        Orig::PARSED.len() + 1,
        OURS_ONLY.len()
    );
}

/// A guard on the guard: the evaluator must really reproduce the C on the forms these files use, or a comparison that passes
/// says nothing. Each expectation below is written out by hand from the meaning of the C, not read back from the evaluator.
#[test]
fn the_evaluator_reads_the_originals_forms() {
    let Some(clone) = clone_dir() else {
        eprintln!("skipped: LUCINA_ASAHI_LINUX not set");
        return;
    };
    let m = Model::load(&clone);
    let cases: &[(&str, Orig, u64)] = &[
        ("AIC_INFO", Orig::Aic, 0x0004),              // plain hex
        ("AIC_EVENT_DIE", Orig::Aic, 0xff00_0000),    // GENMASK(31, 24)
        ("AIC2_IRQ_CFG_TARGET", Orig::Aic, 0xf),      // GENMASK(3, 0)
        ("AIC_IPI_SELF", Orig::Aic, 0x8000_0000),     // BIT(31)
        ("AIC_MAX_IRQ", Orig::Aic, 1024),             // 0x400
        ("AIC_VGIC_MI", Orig::Aic, 6),                // an enum member with no initializer
        ("AIC_NR_FIQ", Orig::Aic, 7),                 // and the one after it
        ("AIC_TMR_EL0_PHYS", Orig::Aic, 0), // an enum member naming a macro of another header
        ("PMCR0_IACT", Orig::Pmu, 1 << 11), // a define of a second header
        ("IRQ_TYPE_SENSE_MASK", Orig::LinuxIrq, 0xf), // an enum member written in hex
        ("ARCH_TIMER_CTRL_IT_STAT", Orig::Timer, 4), // (1 << 2)
    ];
    for (name, file, want) in cases {
        let got = m
            .value(name, *file)
            .unwrap_or_else(|e| panic!("`{name}`: {e}"));
        assert_eq!(
            got,
            *want,
            "the evaluator reads `{name}` of {} as {got}, not {want}",
            file.path()
        );
    }
    // the arithmetic forms, straight from a token stream
    let text = |src: &str, want: u64| {
        let toks = tokenize(src).unwrap_or_else(|e| panic!("`{src}`: {e}"));
        let mut env = Vec::new();
        let mut stack = Vec::new();
        let got = m
            .eval_tokens(&toks, Orig::Aic, &mut env, &mut stack)
            .unwrap_or_else(|e| panic!("`{src}`: {e}"));
        assert_eq!(got, want, "`{src}`");
    };
    text("(1 << 4) | (3 << 0)", 0x13);
    text("GENMASK(9, 3)", 0x3f8);
    text("BIT(0)", 1);
    text("((0xF << 3) | (0x3))", 0x7b);
    text("sizeof(u32) * BITS_PER_BYTE", 32);
    text("(u32)0xdeadbeef", 0xdead_beef);
    text("BIT(64)", 0);
    text("1 + 2 * 3", 7);
    text("(1 | 2) & 3", 3);
    text("1 < 2 ? 8 : 9", 8);
    text("~0u & 0xff", 0xff);
    // and it refuses rather than guessing
    let mut env = Vec::new();
    let mut stack = Vec::new();
    let toks = tokenize("sys_reg(3, 5, 15, 0, 0)").unwrap();
    assert!(
        m.eval_tokens(&toks, Orig::Aic, &mut env, &mut stack)
            .is_err(),
        "a macro this test does not model must be an error, not a number"
    );
}
