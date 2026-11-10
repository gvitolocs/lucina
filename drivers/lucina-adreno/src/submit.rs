// SPDX-License-Identifier: GPL-2.0-only
//! What the ring carries for a program's submit: the switch to its table, its command buffers as `CP_INDIRECT_BUFFER`s, and a
//! fence into its own fence page. After Linux's `a6xx_set_pagetable` and `a6xx_submit` (`drivers/gpu/drm/msm/adreno/a6xx_gpu.c`,
//! 7.1) for an A6xx below the 7xx family, without the statistics counters.
//!
//! The command buffers themselves run from the program's own memory as IB1s, unprivileged and under `CP_PROTECT`
//! (`cp::hw_init`), through the program's table: whatever they say, they reach only what that table maps. The kernel never
//! reads them. Only the ring, which only the kernel writes, may switch tables or leave protected mode.

use alloc::vec::Vec;

use crate::{
    gpu_rules::{self, FENCE_IOVA},
    pm4::{self, pkt4, pkt7},
    regs::CP_SCRATCH_2,
};

/// `REG_A6XX_RBBM_PERFCTR_SRAM_INIT_CMD` and `_STATUS`, which Linux writes and waits for around a table switch.
const RBBM_PERFCTR_SRAM_INIT_CMD: u32 = 0x050e;
const RBBM_PERFCTR_SRAM_INIT_STATUS: u32 = 0x050f;
/// `CP_EVENT_WRITE_0_IRQ`.
const EVENT_IRQ: u32 = 1 << 31;
/// `WRITE_EQ` of `cp_cond_function`.
const WRITE_EQ: u32 = 3;

/// One command buffer as the program names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandBuffer {
    pub bo: u32,
    pub offset: u64,
    pub dwords: u64,
}

/// Why a submit was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    NoCommandBuffers,
    TooManyCommandBuffers,
    NoSuchObject(u32),
    OutsideObject(u32),
}

/// Each command buffer's GPU address and length, if every one lies inside an object of the program. `object` answers a
/// handle with that object's `(iova, size)`, which [`gpu_rules::user_range`] accepted when it was mapped.
pub fn resolve(cmds: &[CommandBuffer], object: impl Fn(u32) -> Option<(u64, u64)>) -> Result<Vec<(u64, u32)>, Refusal> {
    if cmds.is_empty() {
        return Err(Refusal::NoCommandBuffers);
    }
    if cmds.len() as u64 > gpu_rules::MAX_COMMAND_BUFFERS {
        return Err(Refusal::TooManyCommandBuffers);
    }
    let mut out = Vec::with_capacity(cmds.len());
    for c in cmds {
        let (iova, size) = object(c.bo).ok_or(Refusal::NoSuchObject(c.bo))?;
        if !gpu_rules::user_range(iova, size) {
            return Err(Refusal::NoSuchObject(c.bo));
        }
        let at = gpu_rules::command_buffer(iova, size, c.offset, c.dwords).ok_or(Refusal::OutsideObject(c.bo))?;
        out.push((at, c.dwords as u32));
    }
    Ok(out)
}

/// `a6xx_set_pagetable` for an A6xx below 7xx: protected mode off for the SRAM register, the table update (ASID 0, context
/// bank 0, `CONTEXTIDR` 0, as Linux), a UCHE invalidate, the wait for the SRAM, protected mode on. A fault is put down to a
/// program by the `TTBR0` the bank holds when it faults.
///
/// Left out: Linux's `CP_MEM_WRITE` of the new TTBR into its memptrs ("good for debugging, needed for preemption"). Linux's
/// memptrs are in TTBR1, mapped in every table; here the write would come after the switch, into a page the new table may
/// not map, and this driver has no preemption.
pub fn switch_table(words: &mut Vec<u32>, ttbr0: u64) {
    words.extend_from_slice(&[
        pkt7(pm4::CP_SET_PROTECTED_MODE, 1),
        0,
        pkt4(RBBM_PERFCTR_SRAM_INIT_CMD, 1),
        1,
        pkt7(pm4::CP_SMMU_TABLE_UPDATE, 4),
        ttbr0 as u32,
        // TTBR0_HI is bits 0..15, ASID 16..31 (0).
        ((ttbr0 >> 32) as u32) & 0xffff,
        0,
        0,
        pkt7(pm4::CP_EVENT_WRITE, 1),
        pm4::EVENT_CACHE_INVALIDATE,
        pkt7(pm4::CP_WAIT_REG_MEM, 6),
        WRITE_EQ,
        RBBM_PERFCTR_SRAM_INIT_STATUS,
        0,
        1,
        1,
        0,
        pkt7(pm4::CP_SET_PROTECTED_MODE, 1),
        1,
    ]);
}

/// Words [`switch_table`] adds.
pub const SWITCH_WORDS: usize = 20;

/// The body of `a6xx_submit`: CCU depth and colour invalidated, then each command buffer as an IB1.
pub fn command_buffers(words: &mut Vec<u32>, ibs: &[(u64, u32)]) {
    words.extend_from_slice(&[
        pkt7(pm4::CP_EVENT_WRITE, 1),
        pm4::EVENT_PC_CCU_INVALIDATE_DEPTH,
        pkt7(pm4::CP_EVENT_WRITE, 1),
        pm4::EVENT_PC_CCU_INVALIDATE_COLOR,
    ]);
    for &(iova, dwords) in ibs {
        words.extend_from_slice(&[pkt7(pm4::CP_INDIRECT_BUFFER, 3), iova as u32, (iova >> 32) as u32, dwords & 0xf_ffff]);
    }
}

/// The end of `a6xx_submit`: the sequence into `CP_SCRATCH_2`, then `CACHE_FLUSH_TS` writes it to `fence` once everything
/// before it is in memory, and interrupts.
pub fn fence(words: &mut Vec<u32>, fence: u64, sequence: u32) {
    words.extend_from_slice(&[
        pkt4(CP_SCRATCH_2, 1),
        sequence,
        pkt7(pm4::CP_EVENT_WRITE, 4),
        pm4::EVENT_CACHE_FLUSH_TS | EVENT_IRQ,
        fence as u32,
        (fence >> 32) as u32,
        sequence,
    ]);
}

/// A whole submit of a program: switched to its table unless it is the active one, its command buffers, its fence at
/// [`FENCE_IOVA`] in its own table.
pub fn program(words: &mut Vec<u32>, switch: Option<u64>, ibs: &[(u64, u32)], sequence: u32) {
    if let Some(ttbr0) = switch {
        switch_table(words, ttbr0);
    }
    command_buffers(words, ibs);
    fence(words, FENCE_IOVA, sequence);
}

/// Ring words a submit of `ibs` command buffers takes at most.
pub const fn words_for(ibs: usize) -> usize {
    SWITCH_WORDS + 4 + 4 * ibs + 7
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use crate::gpu_rules::BO_BASE;

    /// Splits words into packets by their headers, as the CP reads them.
    fn packets(words: &[u32]) -> Vec<(u32, u32, Vec<u32>)> {
        let mut out = Vec::new();
        let mut at = 0;
        while at < words.len() {
            let h = words[at];
            let (kind, key, count) = match h >> 28 {
                7 => (7, (h >> 16) & 0x7f, h & 0x3fff),
                4 => (4, (h >> 8) & 0x3ffff, h & 0x7f),
                _ => panic!("not a type 4 or 7 header: {:#x}", h),
            };
            out.push((kind, key, words[at + 1..at + 1 + count as usize].to_vec()));
            at += 1 + count as usize;
        }
        assert_eq!(at, words.len());
        out
    }

    #[test]
    fn a_switch_is_linuxs_sequence() {
        let mut w = Vec::new();
        switch_table(&mut w, 0x1_2345_6000);
        assert_eq!(w.len(), SWITCH_WORDS);
        let p = packets(&w);
        let keys: Vec<(u32, u32)> = p.iter().map(|(k, o, _)| (*k, *o)).collect();
        assert_eq!(
            keys,
            [(7, 0x5f), (4, 0x50e), (7, 0x53), (7, 0x46), (7, 0x3c), (7, 0x5f)]
        );
        assert_eq!(p[0].2, [0]);
        assert_eq!(p[2].2, [0x2345_6000, 0x1, 0, 0]);
        assert_eq!(p[3].2, [49]);
        assert_eq!(p[4].2, [3, 0x50f, 0, 1, 1, 0]);
        assert_eq!(p[5].2, [1]);
    }

    #[test]
    fn a_submit_is_ibs_then_the_fence_in_the_programs_page() {
        let mut w = Vec::new();
        program(&mut w, None, &[(BO_BASE + 0x40, 12), (BO_BASE + 0x1000, 3)], 0x55);
        assert!(w.len() <= words_for(2));
        let p = packets(&w);
        assert_eq!(p[2], (7, 0x3f, std::vec![(BO_BASE + 0x40) as u32, (BO_BASE >> 32) as u32, 12]));
        assert_eq!(p[3].1, 0x3f);
        let last = p.last().unwrap();
        assert_eq!(last.1, 0x46);
        assert_eq!(last.2, [4 | (1 << 31), FENCE_IOVA as u32, (FENCE_IOVA >> 32) as u32, 0x55]);
        let mut w = Vec::new();
        program(&mut w, Some(0x9000_0000), &[(BO_BASE, 1)], 1);
        assert!(w.len() <= words_for(1));
        assert_eq!(packets(&w)[2].1, 0x53);
    }

    #[test]
    fn command_buffers_are_resolved_inside_their_objects_only() {
        let objects = |h: u32| match h {
            1 => Some((BO_BASE, 0x1000)),
            2 => Some((BO_BASE + 0x10_0000, 0x4000)),
            9 => Some((0x9bc0_0000, 0x1000)),
            _ => None,
        };
        let ok = resolve(&[CommandBuffer { bo: 2, offset: 0x100, dwords: 4 }, CommandBuffer { bo: 1, offset: 0, dwords: 1024 }], objects);
        assert_eq!(ok, Ok(std::vec![(BO_BASE + 0x10_0100, 4), (BO_BASE, 1024)]));
        assert_eq!(resolve(&[CommandBuffer { bo: 1, offset: 0, dwords: 1025 }], objects), Err(Refusal::OutsideObject(1)));
        assert_eq!(resolve(&[CommandBuffer { bo: 3, offset: 0, dwords: 1 }], objects), Err(Refusal::NoSuchObject(3)));
        // An object outside the window (it never is) still refuses.
        assert_eq!(resolve(&[CommandBuffer { bo: 9, offset: 0, dwords: 1 }], objects), Err(Refusal::NoSuchObject(9)));
        assert_eq!(resolve(&[], objects), Err(Refusal::NoCommandBuffers));
        let many = std::vec![CommandBuffer { bo: 1, offset: 0, dwords: 1 }; 65];
        assert_eq!(resolve(&many, objects), Err(Refusal::TooManyCommandBuffers));
    }
}
