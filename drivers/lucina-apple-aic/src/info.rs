// SPDX-License-Identifier: GPL-2.0-or-later
// Derived from drivers/irqchip/irq-apple-aic.c of AsahiLinux/linux (branch asahi, rev 6ba0637d1ed9):
// Copyright The Asahi Linux Contributors; based on irq-lpc32xx (Copyright 2015-2016 Vladimir Zapolskiy) and
// irq-bcm2836 (Copyright 2015 Broadcom). Behaviour re-expressed in Rust for LucinaOS; see the crate README.
// This is a Rust port of the behaviour of drivers/irqchip/irq-apple-aic.c; the original is the source of truth.

//! Which AIC a device-tree node is (`struct aic_info` and `aic_info_match`, irq-apple-aic.c:246-331), and the register
//! geometry the probe derives from the capability registers (irq-apple-aic.c:995-1049).

use crate::mmio::AicMmio;
use crate::regs::*;

/// The AIC generation (`aic_info.version`, irq-apple-aic.c:247).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Version {
    V1,
    V2,
    V3,
}

/// `struct aic_info` as matched, before the probe fills in the derived offsets (irq-apple-aic.c:246-263).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AicInfo {
    pub version: Version,
    /// Offset of the event register in the event window (`.event`): `AIC_EVENT` on v1, 0 on v2/v3 (field left zero in
    /// `aic2_info`/`aic3_info`, irq-apple-aic.c:291-307).
    pub event: u32,
    /// `.target_cpu`: `AIC_TARGET_CPU` on v1, 0 (absent) on v2/v3.
    pub target_cpu: u32,
    /// `.irq_cfg`: start of the per-die blocks on v2/v3, 0 on v1.
    pub irq_cfg: u32,
    /// `.fast_ipi`: IPIs through the IMP-DEF `IPI_RR_*` / `IPI_SR` system registers; becomes the `use_fast_ipi` static key
    /// (irq-apple-aic.c:241-242, disabled at :1043-1044).
    pub fast_ipi: bool,
    /// `.local_fast_ipi`: `IPI_RR_LOCAL` exists for same-cluster IPIs; becomes `use_local_fast_ipi` (:243-244, :1046-1047).
    pub local_fast_ipi: bool,
}

/// `aic1_info` (irq-apple-aic.c:265-270).
pub const AIC1_INFO: AicInfo = AicInfo {
    version: Version::V1,
    event: AIC_EVENT,
    target_cpu: AIC_TARGET_CPU,
    irq_cfg: 0,
    fast_ipi: false,
    local_fast_ipi: false,
};

/// `aic1_fipi_info` (irq-apple-aic.c:272-279).
pub const AIC1_FIPI_INFO: AicInfo = AicInfo {
    fast_ipi: true,
    ..AIC1_INFO
};

/// `aic1_local_fipi_info` (irq-apple-aic.c:281-289).
pub const AIC1_LOCAL_FIPI_INFO: AicInfo = AicInfo {
    fast_ipi: true,
    local_fast_ipi: true,
    ..AIC1_INFO
};

/// `aic2_info` (irq-apple-aic.c:291-298).
pub const AIC2_INFO: AicInfo = AicInfo {
    version: Version::V2,
    event: 0,
    target_cpu: 0,
    irq_cfg: AIC2_IRQ_CFG,
    fast_ipi: true,
    local_fast_ipi: true,
};

/// `aic3_info` (irq-apple-aic.c:300-307): the same as v2 but for the block base.
pub const AIC3_INFO: AicInfo = AicInfo {
    version: Version::V3,
    irq_cfg: AIC3_IRQ_CFG,
    ..AIC2_INFO
};

/// `aic_info_match` (irq-apple-aic.c:309-331), in table order.
pub const AIC_INFO_MATCH: [(&str, AicInfo); 5] = [
    ("apple,t8103-aic", AIC1_LOCAL_FIPI_INFO),
    ("apple,t8015-aic", AIC1_FIPI_INFO),
    ("apple,aic", AIC1_INFO),
    ("apple,aic2", AIC2_INFO),
    ("apple,t8122-aic3", AIC3_INFO),
];

/// The compatibles the driver is declared for (`IRQCHIP_DECLARE`, irq-apple-aic.c:1134-1136): a node with none of these is
/// never probed by it.
pub const DECLARED: [&str; 3] = ["apple,aic", "apple,aic2", "apple,t8122-aic3"];

/// `of_match_node(aic_info_match, node)` (irq-apple-aic.c:987) for a node whose `compatible` list is `compatible`.
///
/// `of_match_node` keeps the table entry with the best score, and a compatible string scores higher the earlier it stands in
/// the node's list (drivers/of/base.c:367, `INT_MAX/2 - (index << 2)`, best kept at :1160-1170). So the node's first string
/// that is in the table wins. The M4 node lists `"apple,t8132-aic3", "apple,t8122-aic3"` (t8132.dtsi:209): `t8132` is not
/// in the table, and `t8122` gives [`AIC3_INFO`]. A list holding only `"apple,t8132-aic3"` matches nothing, as in the C.
pub fn match_compatible(compatible: &[&str]) -> Option<AicInfo> {
    compatible.iter().find_map(|c| {
        AIC_INFO_MATCH
            .iter()
            .find(|(name, _)| name == c)
            .map(|(_, info)| *info)
    })
}

/// Whether the driver's init runs for this node at all (irq-apple-aic.c:1134-1136).
pub fn is_declared(compatible: &[&str]) -> bool {
    compatible.iter().any(|c| DECLARED.contains(c))
}

/// The capability registers the probe reads, per generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Caps {
    /// v1: `AIC_INFO` (irq-apple-aic.c:999).
    V1 { aic_info: u32 },
    /// v2/v3: `AIC2_INFO1` and `AIC2_INFO3` (irq-apple-aic.c:1014-1015).
    V2 { info1: u32, info3: u32 },
}

/// Reads the capability registers of `info`'s generation from the core window, in the C's order (irq-apple-aic.c:999,
/// :1014-1015).
pub fn read_caps<M: AicMmio>(info: &AicInfo, core: &M) -> Caps {
    match info.version {
        Version::V1 => Caps::V1 {
            aic_info: core.read32(AIC_INFO as usize),
        },
        Version::V2 | Version::V3 => {
            let info1 = core.read32(AIC2_INFO1 as usize);
            let info3 = core.read32(AIC2_INFO3 as usize);
            Caps::V2 { info1, info3 }
        }
    }
}

/// What the probe derives: the counts (`nr_irq`, `max_irq`, `nr_die`, `max_die` of `struct aic_irq_chip`,
/// irq-apple-aic.c:341-344) and the per-die offsets it writes into `aic_info` (:1033-1049).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Geometry {
    pub nr_irq: u32,
    pub max_irq: u32,
    pub nr_die: u32,
    /// Only printed by the C (irq-apple-aic.c:1119-1120); nothing depends on it.
    pub max_die: u32,
    /// Offset of the per-die block start: `start_off` (irq-apple-aic.c:1004, :1022).
    pub start_off: u32,
    pub sw_set: u32,
    pub sw_clr: u32,
    pub mask_set: u32,
    pub mask_clr: u32,
    /// Offset of HW_STATE (the C skips over it without naming it, irq-apple-aic.c:1041).
    pub hw_state: u32,
    pub die_stride: u32,
    /// The event register is in the core window (v1, `irqc->event = irqc->base`, irq-apple-aic.c:1007) rather than in the
    /// node's second `reg` entry (v2/v3, `of_iomap(node, 1)`, :1025).
    pub event_in_core: bool,
}

/// The geometry the probe computes from `caps` (irq-apple-aic.c:995-1049). `None` if `caps` is not `info`'s generation.
///
/// v1: `nr_irq` from `AIC_INFO`, `max_irq` = `AIC_MAX_IRQ`, one die, the blocks start after `TARGET_CPU[max_irq]`
/// (:996-1010). v2/v3: `nr_irq` from `INFO1`, `max_irq` from `INFO3`, `nr_die` = `LAST_DIE` + 1, `max_die` from `INFO3`,
/// the blocks start after `IRQ_CFG[max_irq]` (:1011-1030). Then SW_SET, SW_CLR, MASK_SET, MASK_CLR and HW_STATE follow, each
/// `max_irq >> 5` words (:1033-1041), and `die_stride` is the whole run from `start_off` (:1049). All in u32, as the C.
pub fn derive_geometry(info: &AicInfo, caps: Caps) -> Option<Geometry> {
    let (nr_irq, max_irq, nr_die, max_die, start_off, event_in_core) = match (info.version, caps) {
        (Version::V1, Caps::V1 { aic_info }) => {
            // irq-apple-aic.c:1000-1004
            (
                field_get(AIC_INFO_NR_IRQ, aic_info),
                AIC_MAX_IRQ,
                1,
                1,
                info.target_cpu,
                true,
            )
        }
        (Version::V2 | Version::V3, Caps::V2 { info1, info3 }) => (
            // irq-apple-aic.c:1017-1022
            field_get(AIC2_INFO1_NR_IRQ, info1),
            field_get(AIC2_INFO3_MAX_IRQ, info3),
            field_get(AIC2_INFO1_LAST_DIE, info1) + 1,
            field_get(AIC2_INFO3_MAX_DIE, info3),
            info.irq_cfg,
            false,
        ),
        _ => return None,
    };
    // irq-apple-aic.c:1005 (TARGET_CPU) and :1023 (IRQ_CFG): one u32 per IRQ up to max_irq.
    let words = 4u32.wrapping_mul(max_irq >> 5);
    let mut off = start_off.wrapping_add(4u32.wrapping_mul(max_irq));
    let sw_set = off; // :1033
    off = off.wrapping_add(words); // :1034
    let sw_clr = off; // :1035
    off = off.wrapping_add(words); // :1036
    let mask_set = off; // :1037
    off = off.wrapping_add(words); // :1038
    let mask_clr = off; // :1039
    off = off.wrapping_add(words); // :1040
    let hw_state = off;
    off = off.wrapping_add(words); // :1041
    Some(Geometry {
        nr_irq,
        max_irq,
        nr_die,
        max_die,
        start_off,
        sw_set,
        sw_clr,
        mask_set,
        mask_clr,
        hw_state,
        die_stride: off.wrapping_sub(start_off), // :1049
        event_in_core,
    })
}
