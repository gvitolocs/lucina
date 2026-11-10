// SPDX-License-Identifier: GPL-2.0-only
//! The GPU's own SMMU, `qcom,adreno-smmu` at 0x5040000 (SMMUv2, 8 context banks), behind the CX switch. Stream 0 is the GPU,
//! stream 5 the GMU. Only what this driver needs of Linux's `arm-smmu.c` and `io-pgtable-arm.c`: the GMU's firmware can reach
//! only addresses below 0x80000000, RAM is elsewhere, so its stream has to translate. The GPU's goes straight through.

use alloc::vec::Vec;

use lucina_qcom_soc::io::RegIo;

use crate::{Hal, Result, err, poll};

pub const STREAM_GPU: u32 = 0;
pub const STREAM_GMU: u32 = 5;
/// Linux keeps context bank 0 for the GPU (`qcom_adreno_smmu_alloc_context_bank`).
pub const BANK_GPU: u32 = 0;
pub const BANK_GMU: u32 = 1;

const SCR0: u32 = 0x00;
const ID0: u32 = 0x20;
const ID1: u32 = 0x24;
const ID2: u32 = 0x28;
const GFSR: u32 = 0x48;
const GFSYNR0: u32 = 0x50;
const GFSYNR1: u32 = 0x54;
const TLBIALLNSNH: u32 = 0x68;
const TLBIALLH: u32 = 0x6c;
const TLBGSYNC: u32 = 0x70;
const TLBGSTATUS: u32 = 0x74;
const SMR0: u32 = 0x800;
const S2CR0: u32 = 0xc00;

const SMR_VALID: u32 = 1 << 31;
const S2CR_TYPE_SHIFT: u32 = 16;
const S2CR_TYPE_TRANS: u32 = 0;

// Context bank registers.
const CB_SCTLR: u32 = 0x00;
const CB_TCR2: u32 = 0x10;
const CB_TTBR0: u32 = 0x20;
const CB_TCR: u32 = 0x30;
const CB_MAIR0: u32 = 0x38;
const CB_MAIR1: u32 = 0x3c;
const CB_FSR: u32 = 0x58;
const CB_FAR: u32 = 0x60;
const CB_FSYNR0: u32 = 0x68;
/// `ARM_SMMU_CB_RESUME`: a stalled transaction goes on (0) or is terminated (`ARM_SMMU_RESUME_TERMINATE`, 1).
const CB_RESUME: u32 = 0x8;
const RESUME_TERMINATE: u32 = 1;
/// `ARM_SMMU_CB_FSR_SS`: the faulting transaction is stalled.
const FSR_SS: u32 = 1 << 30;
const CB_TLBIALL: u32 = 0x618;
const CB_TLBIASID: u32 = 0x610;
const CB_TLBSYNC: u32 = 0x7f0;
const CB_TLBSTATUS: u32 = 0x7f4;

/// `FSR.FORMAT`, which says AArch64 on an idle bank: not a fault.
const FSR_FORMAT: u32 = 3 << 9;

const SCTLR_M: u32 = 1 << 0;
const SCTLR_TRE: u32 = 1 << 1;
const SCTLR_AFE: u32 = 1 << 2;
const SCTLR_CFRE: u32 = 1 << 5;
const SCTLR_CFIE: u32 = 1 << 6;
/// `ARM_SMMU_SCTLR_HUPCF`: transactions after a fault go on, so that a bad address of a program's never hangs the GPU
/// (`qcom_adreno_smmu_write_sctlr`). Stalling (`CFCFG`) stays off: a faulting access is terminated.
const SCTLR_HUPCF: u32 = 1 << 8;
/// `ARM_SMMU_SCTLR_CFCFG`: a faulting transaction stalls until the fault handler resumes it (`qcom_adreno_smmu_set_stall`).
const SCTLR_CFCFG: u32 = 1 << 7;

/// `ARM_SMMU_sCR0_*` of `arm-smmu.h`.
const SCR0_CLIENTPD: u32 = 1 << 0;
const SCR0_GFRE: u32 = 1 << 1;
const SCR0_GFIE: u32 = 1 << 2;
const SCR0_GCFGFRE: u32 = 1 << 4;
const SCR0_GCFGFIE: u32 = 1 << 5;
const SCR0_USFCFG: u32 = 1 << 10;
const SCR0_VMIDPNE: u32 = 1 << 11;
const SCR0_PTM: u32 = 1 << 12;
const SCR0_FB: u32 = 1 << 13;
const SCR0_BSU: u32 = 0x3 << 14;

/// `S1_TRANS_S2_BYPASS`, bypass non-shareable, write-back.
const CBAR_S1: u32 = (1 << 16) | (3 << 8) | (0xf << 12);
/// VMID 0 is not used by Linux.
const fn cbar(bank: u32) -> u32 {
    CBAR_S1 | (bank + 1)
}
/// `ARM_SMMU_CBA2R_VA64`.
const CBA2R_VA64: u32 = 1;

/// What one stream does now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamState {
    pub smr_index: Option<u32>,
    pub s2cr: u32,
}

/// The registers, through one window.
pub struct Smmu<R> {
    io: R,
    /// `ID1.PAGESIZE`: 64 KiB or 4 KiB register pages.
    page: u32,
    num_page_ndxb: u32,
    pub num_banks: u32,
    pub num_smr: u32,
}

/// One stage-1 page table: 4 KiB pages under a 4 GiB space, in physically contiguous memory.
pub struct PageTable {
    /// Words of the first table (level 1), then one level 2 and one level 3 table per block used.
    pub words: Vec<u64>,
}

/// `ARM_LPAE_PTE_*`: page, access flag, non-global, outer shareable, normal non-cached (`attr` 0), read-write at any level.
const PTE_PAGE: u64 = 3 | (1 << 10) | (1 << 11) | (2 << 8) | (1 << 6);
const PTE_TABLE: u64 = 3;

impl PageTable {
    /// Maps `pages` pages of `phys`.. at `iova`.., inside one 2 MiB block of a 4 GiB space. `table_phys` is where these words
    /// will be. Fails if the range crosses that block.
    pub fn one_range(table_phys: u64, iova: u64, phys: u64, pages: u64) -> Result<Self> {
        const TABLE_WORDS: usize = 512;
        if iova >= 1 << 32 || (iova & 0xfff) != 0 || (phys & 0xfff) != 0 {
            return err("page table: bad address", iova as u32);
        }
        let l1 = ((iova >> 30) & 3) as usize;
        let l2 = ((iova >> 21) & 0x1ff) as usize;
        let l3 = ((iova >> 12) & 0x1ff) as usize;
        if l3 as u64 + pages > TABLE_WORDS as u64 {
            return err("page table: range crosses a 2 MiB block", pages as u32);
        }
        let mut words = alloc::vec![0u64; TABLE_WORDS * 3];
        words[l1] = (table_phys + 0x1000) | PTE_TABLE;
        words[TABLE_WORDS + l2] = (table_phys + 0x2000) | PTE_TABLE;
        for page in 0..pages as usize {
            words[2 * TABLE_WORDS + l3 + page] = (phys + (page as u64) * 0x1000) | PTE_PAGE;
        }
        Ok(Self { words })
    }

    /// `walk` of the hardware, for tests and for the log.
    pub fn translate(&self, table_phys: u64, iova: u64) -> Option<u64> {
        let read = |phys: u64, index: u64| {
            let at = ((phys - table_phys) / 8 + index) as usize;
            self.words.get(at).copied()
        };
        let l1 = read(table_phys, (iova >> 30) & 3)?;
        if l1 & 3 != 3 {
            return None;
        }
        let l2 = read(l1 & 0xffff_ffff_f000, (iova >> 21) & 0x1ff)?;
        if l2 & 3 != 3 {
            return None;
        }
        let l3 = read(l2 & 0xffff_ffff_f000, (iova >> 12) & 0x1ff)?;
        if l3 & 3 != 3 {
            return None;
        }
        Some((l3 & 0xffff_ffff_f000) | (iova & 0xfff))
    }
}

impl<R: RegIo> Smmu<R> {
    /// Reads the identification registers. Runs with CX on and the config clock running.
    pub fn probe(io: R) -> Self {
        let id0 = io.read32(ID0);
        let id1 = io.read32(ID1);
        let page = if id1 & (1 << 31) != 0 {
            0x1_0000
        } else {
            0x1000
        };
        let num_page_ndxb = (id1 >> 28) & 7;
        let num_banks = id1 & 0xff;
        let num_smr = id0 & 0xff;
        Self {
            io,
            page,
            num_page_ndxb,
            num_banks,
            num_smr,
        }
    }

    pub fn ids(&self) -> (u32, u32, u32) {
        (
            self.io.read32(ID0),
            self.io.read32(ID1),
            self.io.read32(ID2),
        )
    }

    pub fn scr0(&self) -> u32 {
        self.io.read32(SCR0)
    }

    /// Page 1 of the global space.
    fn gr1(&self) -> u32 {
        self.page
    }

    /// `ARM_SMMU_CB`: context banks start after the global half, `1 << (NUMPAGENDXB + 1)` pages in.
    fn bank(&self, bank: u32) -> u32 {
        self.page * (1 << (self.num_page_ndxb + 1)) + bank * self.page
    }

    /// Which SMR, if any, catches a stream, and what its S2CR says.
    pub fn stream(&self, sid: u32) -> StreamState {
        for index in 0..self.num_smr {
            let smr = self.io.read32(SMR0 + index * 4);
            let (id, mask) = (smr & 0xffff, (smr >> 16) & 0x7fff);
            if smr & SMR_VALID != 0 && (sid & !mask) == (id & !mask) {
                return StreamState {
                    smr_index: Some(index),
                    s2cr: self.io.read32(S2CR0 + index * 4),
                };
            }
        }
        StreamState {
            smr_index: None,
            s2cr: 0,
        }
    }

    /// `(SCTLR, TTBR0, FSR, FAR)` of a bank.
    pub fn bank_state(&self, bank: u32) -> (u32, u64, u32, u64) {
        let b = self.bank(bank);
        let read64 = |off: u32| {
            u64::from(self.io.read32(b + off)) | (u64::from(self.io.read32(b + off + 4)) << 32)
        };
        (
            self.io.read32(b + CB_SCTLR),
            read64(CB_TTBR0),
            self.io.read32(b + CB_FSR) & !FSR_FORMAT,
            read64(CB_FAR),
        )
    }

    /// The first SMR that is free, or the one that already catches this stream.
    fn smr_for(&self, sid: u32) -> Result<u32> {
        if let Some(index) = self.stream(sid).smr_index {
            return Ok(index);
        }
        for index in 0..self.num_smr {
            if self.io.read32(SMR0 + index * 4) & SMR_VALID == 0 {
                return Ok(index);
            }
        }
        err("SMMU: no free stream match register", self.num_smr)
    }

    fn map_stream(&self, sid: u32, s2cr: u32) -> Result<()> {
        let index = self.smr_for(sid)?;
        // S2CR before SMR, as `arm_smmu_write_sme`, so that the stream is never caught by a stale entry.
        self.io.write32(S2CR0 + index * 4, s2cr);
        self.io.write32(SMR0 + index * 4, SMR_VALID | sid);
        Ok(())
    }

    /// Lets a stream straight through. A bypass entry reads back as FAULT on the laptop; Linux's Adreno SMMU variant does
    /// not run `qcom_smmu_cfg_probe`, so it never writes a bypass S2CR for the GPU stream and always gives it a translated
    /// bank. This is the same answer: a context bank with stage 1 off (`SCTLR` 0), bypassing as non-shareable write-back.
    pub fn pass_stream<H: Hal>(&self, hal: &H, sid: u32, bank: u32) -> Result<()> {
        if bank >= self.num_banks {
            return err("SMMU: no such context bank", bank);
        }
        let b = self.bank(bank);
        let gr1 = self.gr1();
        self.io.write32(b + CB_SCTLR, 0);
        self.io.write32(b + CB_FSR, u32::MAX);
        self.io.write32(gr1 + 0x800 + bank * 4, CBA2R_VA64);
        self.io.write32(gr1 + bank * 4, CBAR_S1);
        hal.barrier();
        self.map_stream(sid, (S2CR_TYPE_TRANS << S2CR_TYPE_SHIFT) | bank)?;
        let seen = self.stream(sid).s2cr;
        if (seen >> S2CR_TYPE_SHIFT) & 3 != S2CR_TYPE_TRANS || seen & 0xff != bank {
            return err("SMMU: the stream mapping did not stick", seen);
        }
        hal.barrier();
        Ok(())
    }

    /// `arm_smmu_write_context_bank` for stage 1 under a 4 GiB space, walking non-cached.
    pub fn translate_stream<H: Hal>(&self, hal: &H, sid: u32, bank: u32, ttbr0: u64) -> Result<()> {
        self.translate_stream_as(hal, sid, bank, ttbr0, 32, 0)
    }

    /// The GPU's own stream on its bank: stage 1 over [`crate::gpu_rules::VA_BITS`] (`T0SZ` 25, a walk from level 1), with
    /// `HUPCF` as Linux's Adreno SMMU sets it, ASID 0 for every table (`msm_iommu_pagetable_params`). `ttbr0` is the kernel's
    /// table; the ring switches it with `CP_SMMU_TABLE_UPDATE`.
    ///
    /// `stall`: a fault stalls the access until its handler has read it (patch 1173): only with the fault interrupt wired, or
    /// the GPU would wait for ever.
    pub fn translate_gpu<H: Hal>(&self, hal: &H, ttbr0: u64, stall: bool) -> Result<()> {
        let extra = SCTLR_HUPCF | if stall { SCTLR_CFCFG } else { 0 };
        self.translate_stream_as(hal, STREAM_GPU, BANK_GPU, ttbr0, crate::gpu_rules::T0SZ, extra)
    }

    /// `qcom_adreno_smmu_set_stall` for the GPU's bank, as it runs.
    pub fn set_gpu_stall<H: Hal>(&self, hal: &H, stall: bool) {
        let b = self.bank(BANK_GPU);
        let sctlr = self.io.read32(b + CB_SCTLR);
        self.io.write32(b + CB_SCTLR, if stall { sctlr | SCTLR_CFCFG } else { sctlr & !SCTLR_CFCFG });
        hal.barrier();
    }

    fn translate_stream_as<H: Hal>(&self, hal: &H, sid: u32, bank: u32, ttbr0: u64, t0sz: u32, sctlr: u32) -> Result<()> {
        if bank >= self.num_banks {
            return err("SMMU: no such context bank", bank);
        }
        let b = self.bank(bank);
        let gr1 = self.gr1();
        self.io.write32(b + CB_SCTLR, 0);
        self.io.write32(b + CB_FSR, u32::MAX);
        self.io.write32(gr1 + 0x800 + bank * 4, CBA2R_VA64);
        self.io.write32(gr1 + bank * 4, cbar(bank));
        // `arm_smmu_lpae_tcr`: T0SZ 32, TG0 4 KiB, walks non-cached and outer shareable, TTBR1 off.
        let tcr: u32 = t0sz | (2 << 12) | (1 << 23);
        // `SEP_UPSTREAM`, PASIZE 1: this SMMU reports a 36-bit output address size (`ID2.OAS` = 1).
        let tcr2: u32 = (7 << 15) | 1;
        self.io.write32(b + CB_TCR2, tcr2);
        self.io.write32(b + CB_TCR, tcr);
        let with_asid = ttbr0 | (u64::from(bank) << 48);
        self.io.write32(b + CB_TTBR0, with_asid as u32);
        self.io.write32(b + CB_TTBR0 + 4, (with_asid >> 32) as u32);
        // `ARM_LPAE_MAIR_ATTR_NC` is attribute 0.
        self.io.write32(b + CB_MAIR0, 0x0004_ff44);
        self.io.write32(b + CB_MAIR1, 0);
        hal.barrier();
        self.io.write32(
            b + CB_SCTLR,
            SCTLR_M | SCTLR_TRE | SCTLR_AFE | SCTLR_CFRE | SCTLR_CFIE | sctlr,
        );
        hal.barrier();
        self.io.write32(b + CB_TLBIASID, bank);
        self.io.write32(b + CB_TLBSYNC, 0);
        poll(
            hal,
            "SMMU: bank TLB sync",
            10_000,
            1,
            || self.io.read32(b + CB_TLBSTATUS),
            |v| v & 1 == 0,
        )?;
        self.map_stream(sid, (S2CR_TYPE_TRANS << S2CR_TYPE_SHIFT) | bank)?;
        hal.barrier();
        Ok(())
    }

    /// Invalidates what the firmware may have left, and turns client ports on: what Linux's `arm_smmu_device_reset` does to
    /// sCR0, without a reset of the stream mappings and context banks.
    pub fn enable<H: Hal>(&self, hal: &H) -> Result<()> {
        self.io.write32(GFSR, self.io.read32(GFSR));
        self.io.write32(TLBIALLH, 0);
        self.io.write32(TLBIALLNSNH, 0);
        self.io.write32(TLBGSYNC, 0);
        poll(
            hal,
            "SMMU: global TLB sync",
            10_000,
            1,
            || self.io.read32(TLBGSTATUS),
            |v| v & 1 == 0,
        )?;
        // GFRE | GFIE | GCFGFRE | GCFGFIE | VMIDPNE | PTM set; CLIENTPD, USFCFG, FB and BSU cleared.
        let scr0 = self.io.read32(SCR0);
        let enabled = (scr0 & !(SCR0_CLIENTPD | SCR0_USFCFG | SCR0_FB | SCR0_BSU))
            | SCR0_GFRE
            | SCR0_GFIE
            | SCR0_GCFGFRE
            | SCR0_GCFGFIE
            | SCR0_VMIDPNE
            | SCR0_PTM;
        self.io.write32(SCR0, enabled);
        hal.barrier();
        Ok(())
    }

    /// Global fault status and its syndromes.
    pub fn global_fault(&self) -> (u32, u32, u32) {
        (
            self.io.read32(GFSR),
            self.io.read32(GFSYNR0),
            self.io.read32(GFSYNR1),
        )
    }

    /// Whether the GPU's bank translates through `ttbr0`, as [`Self::translate_gpu`] left it, and its stream reaches it.
    pub fn gpu_translates(&self, ttbr0: u64) -> bool {
        let (sctlr, now, _, _) = self.bank_state(BANK_GPU);
        sctlr & SCTLR_M != 0 && sctlr & SCTLR_HUPCF != 0 && now & ((1 << 48) - 1) == ttbr0 && self.stream(STREAM_GPU).smr_index.is_some()
    }

    /// Drops every translation the bank's TLB holds (`TLBIALL` of the context bank, then a sync): after a mapping went and
    /// before its memory is used for anything else.
    pub fn invalidate<H: Hal>(&self, hal: &H, bank: u32) -> Result<()> {
        let b = self.bank(bank);
        hal.barrier();
        self.io.write32(b + CB_TLBIALL, 0);
        self.io.write32(b + CB_TLBSYNC, 0);
        poll(hal, "SMMU: bank TLB sync", 10_000, 1, || self.io.read32(b + CB_TLBSTATUS), |v| v & 1 == 0)?;
        Ok(())
    }

    /// `qcom_adreno_smmu_get_fault_info` and the acknowledge of `arm_smmu_context_fault`, for the fault interrupt: `(FSR, FAR,
    /// FSYNR0, TTBR0)` as they are at the fault, `FSR` written back. `None` if the bank holds no fault (not this line's).
    pub fn take_fault_info(&self, bank: u32) -> Option<(u32, u64, u32, u64)> {
        let b = self.bank(bank);
        let fsr = self.io.read32(b + CB_FSR) & !FSR_FORMAT;
        if fsr == 0 {
            return None;
        }
        let read64 = |off: u32| u64::from(self.io.read32(b + off)) | (u64::from(self.io.read32(b + off + 4)) << 32);
        let (far, syn, ttbr0) = (read64(CB_FAR), self.io.read32(b + CB_FSYNR0), read64(CB_TTBR0));
        self.io.write32(b + CB_FSR, fsr);
        // `arm_smmu_context_fault`: a stalled access is terminated, after FSR is written back.
        if fsr & FSR_SS != 0 {
            self.io.write32(b + CB_RESUME, RESUME_TERMINATE);
        }
        Some((fsr, far, syn, ttbr0))
    }

    /// `(FSR, FAR, FSYNR0)` of a bank; acknowledges them.
    pub fn take_fault(&self, bank: u32) -> Option<(u32, u64, u32)> {
        let b = self.bank(bank);
        let fsr = self.io.read32(b + CB_FSR) & !FSR_FORMAT;
        if fsr == 0 {
            return None;
        }
        let far = u64::from(self.io.read32(b + CB_FAR))
            | (u64::from(self.io.read32(b + CB_FAR + 4)) << 32);
        let syn = self.io.read32(b + CB_FSYNR0);
        self.io.write32(b + CB_FSR, fsr);
        Some((fsr, far, syn))
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use core::cell::{Cell, RefCell};
    use std::vec::Vec as StdVec;

    use super::*;

    struct Trace(RefCell<StdVec<(u32, u32)>>);

    impl RegIo for Trace {
        fn read32(&self, off: u32) -> u32 {
            if off == ID0 {
                return 8;
            }
            if off == ID1 {
                return (1 << 31) | (1 << 28) | 8;
            }
            self.0
                .borrow()
                .iter()
                .rev()
                .find(|(o, _)| *o == off)
                .map(|(_, v)| *v)
                .unwrap_or(0)
        }
        fn write32(&self, off: u32, value: u32) {
            self.0.borrow_mut().push((off, value));
        }
    }

    struct Hal {
        now: Cell<u64>,
    }

    impl crate::Hal for Hal {
        fn now_us(&self) -> u64 {
            let t = self.now.get();
            self.now.set(t + 100);
            t
        }
        fn delay_us(&self, _us: u32) {}
        fn barrier(&self) {}
        fn log(&self, _args: core::fmt::Arguments) {}
    }

    #[test]
    fn the_reset_writes_tlbiallh_and_the_full_scr0_bits() {
        let io = Trace(RefCell::new(StdVec::new()));
        let smmu = Smmu::probe(&io);
        smmu.enable(&Hal { now: Cell::new(0) }).unwrap();
        let trace = io.0.borrow();
        assert!(trace.iter().any(|(o, v)| *o == TLBIALLH && *v == 0));
        assert!(trace.iter().any(|(o, v)| *o == TLBIALLNSNH && *v == 0));
        let scr0 = trace
            .iter()
            .rev()
            .find(|(o, _)| *o == SCR0)
            .map(|(_, v)| *v)
            .unwrap();
        assert_eq!(
            scr0,
            SCR0_GFRE | SCR0_GFIE | SCR0_GCFGFRE | SCR0_GCFGFIE | SCR0_VMIDPNE | SCR0_PTM
        );
        assert_eq!(scr0 & (SCR0_CLIENTPD | SCR0_USFCFG | SCR0_FB | SCR0_BSU), 0);
    }

    #[test]
    fn tcr2_pasize_is_one_for_a_36_bit_smmu() {
        let io = Trace(RefCell::new(StdVec::new()));
        let smmu = Smmu::probe(&io);
        smmu.translate_stream(&Hal { now: Cell::new(0) }, STREAM_GMU, BANK_GMU, 0x9000_0000)
            .unwrap();
        let trace = io.0.borrow();
        let bank = smmu.bank(BANK_GMU);
        let tcr2 = trace
            .iter()
            .find(|(o, _)| *o == bank + CB_TCR2)
            .map(|(_, v)| *v)
            .unwrap();
        assert_eq!(tcr2, (7 << 15) | 1);
    }

    #[test]
    fn one_range_translates_what_it_maps_and_nothing_else() {
        let table = 0x9000_0000;
        let pt = PageTable::one_range(table, 0x6000_0000, 0xa123_4000, 16).unwrap();
        assert_eq!(pt.translate(table, 0x6000_0000), Some(0xa123_4000));
        assert_eq!(pt.translate(table, 0x6000_9abc), Some(0xa123_dabc));
        assert_eq!(pt.translate(table, 0x6001_0000), None);
        assert_eq!(pt.translate(table, 0x5fff_f000), None);
        assert_eq!(pt.translate(table, 0xa000_0000), None);
    }

    #[test]
    fn one_range_refuses_what_a_single_table_cannot_hold() {
        assert!(PageTable::one_range(0, 0x601f_f000, 0, 2).is_err());
        assert!(PageTable::one_range(0, 0x6000_0800, 0, 1).is_err());
    }

    #[test]
    fn the_page_entry_has_linuxs_bits() {
        // Valid, page, attr 0, AP unpriv, outer shareable, AF, nG.
        assert_eq!(PTE_PAGE, 0xe43);
    }
}
