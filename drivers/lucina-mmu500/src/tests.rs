//! Tests against a model of the MMU-500 register file with the Qualcomm behaviour Linux works around: an S2CR
//! BYPASS stored as FAULT, FAULT writes ignored, write-one-to-clear fault registers, SMRs the firmware left.

extern crate std;

use std::{format, string::String, vec, vec::Vec};

use super::*;

const WINDOW: usize = 1 << 20;
const PAGE: usize = 1 << 12;
/// SC7180-like: 4 KiB pages, 128 global pages (CB0 at +0x80000), 128 SMRs, 64 context banks.
const ID0: u32 = (1 << 30) | (1 << 29) | (1 << 28) | (1 << 27) | (1 << 14) | (15 << 9) | 128;
const ID1: u32 = (6 << 28) | 64;
const ID2: u32 = (1 << 12) | (5 << 8) | (5 << 4) | 5;
const ID7: u32 = 0x21;

struct Model {
    regs: Vec<u32>,
    writes: Vec<(usize, u32)>,
    /// The hypervisor stores an S2CR of type BYPASS as FAULT and ignores writes of type FAULT.
    bypass_quirk: bool,
}

impl Model {
    fn new() -> Self {
        let mut m = Model {
            regs: vec![0; WINDOW / 4],
            writes: Vec::new(),
            bypass_quirk: false,
        };
        m.set(0x20, ID0);
        m.set(0x24, ID1);
        m.set(0x28, ID2);
        m.set(0x3c, ID7);
        // Reset S2CRs: FAULT for unmatched entries is what Qualcomm firmware leaves; it does not matter while
        // the SMR is invalid.
        for i in 0..128 {
            m.set(0xc00 + 4 * i, 2 << 16);
        }
        m
    }

    fn set(&mut self, offset: usize, value: u32) {
        self.regs[offset / 4] = value;
    }

    fn get(&self, offset: usize) -> u32 {
        self.regs[offset / 4]
    }

    fn gr1(n: usize) -> usize {
        PAGE + n
    }

    fn cb(bank: usize, offset: usize) -> usize {
        (128 + bank) * PAGE + offset
    }

    /// A valid SMR the firmware left, with its S2CR.
    fn firmware_smr(&mut self, slot: usize, id: u16, mask: u16, s2cr: u32) {
        self.set(
            0x800 + 4 * slot,
            SMR_VALID | (u32::from(mask) << 16) | u32::from(id),
        );
        self.set(0xc00 + 4 * slot, s2cr);
    }

    fn is_w1c(offset: usize) -> bool {
        offset == 0x48 || (offset >= 128 * PAGE && offset % PAGE == 0x58)
    }
}

impl Regs for Model {
    fn read32(&mut self, offset: usize) -> u32 {
        assert!(
            offset < WINDOW && offset.is_multiple_of(4),
            "read outside the window: {offset:#x}"
        );
        self.get(offset)
    }

    fn write32(&mut self, offset: usize, value: u32) {
        assert!(
            offset < WINDOW && offset.is_multiple_of(4),
            "write outside the window: {offset:#x}"
        );
        assert!(
            ![0x000, 0x010].contains(&offset) && !(0x060..0x080).contains(&offset),
            "global register written: {offset:#x}"
        );
        self.writes.push((offset, value));
        if Self::is_w1c(offset) {
            let old = self.get(offset);
            self.set(offset, old & !value);
            return;
        }
        if self.bypass_quirk && (0xc00..0xe00).contains(&offset) {
            match (value >> 16) & 3 {
                1 => return self.set(offset, (value & !(3 << 16)) | (2 << 16)),
                2 => return,
                _ => {}
            }
        }
        self.set(offset, value);
    }
}

fn probe(m: &mut Model) -> Smmu {
    Smmu::probe(m, WINDOW).unwrap()
}

#[test]
fn geometry_from_id_registers() {
    let mut m = Model::new();
    let g = Geometry::read(&mut m);
    assert_eq!(g.smrs, 128);
    assert_eq!(g.banks, 64);
    assert_eq!(g.page_shift, 12);
    assert_eq!(g.global_pages, 128);
    assert_eq!(g.window_size(), WINDOW);
    assert_eq!(
        (
            g.stream_id_bits,
            g.stage1,
            g.stream_matching,
            g.coherent_walk
        ),
        (15, true, true, true)
    );
    assert_eq!(
        (g.ubs_bits, g.ias_bits, g.oas_bits, g.major, g.minor),
        (48, 48, 48, 2, 1)
    );
    assert_eq!(g.cb(3, 0x58), 0x83058);
    assert_eq!(g.gr1(0x400), 0x1400);
}

#[test]
fn probe_only_reads_and_checks_the_window() {
    let mut m = Model::new();
    m.firmware_smr(5, 0x800, 0x2, 1 << 16);
    m.set(Model::cb(63, 0), SCTLR_M);
    let smmu = probe(&mut m);
    assert!(m.writes.is_empty());
    let (_, smr, s2cr) = smmu.smrs().nth(5).unwrap();
    assert_eq!(
        (smr.valid, smr.id, smr.mask, s2cr.typ),
        (true, 0x800, 0x2, S2crType::Bypass)
    );
    assert!(smmu.banks()[63].in_use());
    assert_eq!(
        Smmu::probe(&mut m, WINDOW / 2).err(),
        Some(Error::Window {
            needed: WINDOW,
            size: WINDOW / 2
        })
    );
}

#[test]
fn bypass_on_a_free_smr_writes_s2cr_before_smr() {
    let mut m = Model::new();
    let mut smmu = probe(&mut m);
    let a = smmu.attach_bypass(&mut m, 0xa0, 0).unwrap();
    assert_eq!(
        a,
        Attached {
            slot: 0,
            inherited: false,
            changed: true,
            via: Via::Bypass
        }
    );
    assert_eq!(m.get(0xc00) >> 16 & 3, 1);
    assert_eq!(m.get(0x800), SMR_VALID | 0xa0);
    let s2cr_write = m.writes.iter().rposition(|&(o, _)| o == 0xc00).unwrap();
    let smr_write = m.writes.iter().position(|&(o, _)| o == 0x800).unwrap();
    assert!(s2cr_write < smr_write, "{:?}", m.writes);
    // The second master takes the next slot; the quirk test is not repeated.
    let n = m.writes.len();
    assert_eq!(smmu.attach_bypass(&mut m, 0x540, 0).unwrap().slot, 1);
    assert_eq!(m.writes.len(), n + 2);
}

#[test]
fn quirk_uses_a_stage1_disabled_bank_and_leaves_firmware_banks() {
    let mut m = Model::new();
    m.bypass_quirk = true;
    // The two highest banks belong to someone else: one translating, one pointed at by a firmware S2CR.
    m.set(Model::cb(63, 0), SCTLR_M);
    m.firmware_smr(0, 0x43, 0, 62);
    let mut smmu = probe(&mut m);
    let a = smmu.attach_bypass(&mut m, 0xa0, 0).unwrap();
    assert_eq!(
        a,
        Attached {
            slot: 1,
            inherited: false,
            changed: true,
            via: Via::Bank(61)
        }
    );
    assert_eq!(m.get(Model::gr1(4 * 61)), CBAR_TYPE_S1_TRANS_S2_BYPASS);
    assert_eq!(m.get(Model::cb(61, 0)), 0);
    assert_eq!(m.get(0xc04), 61, "TRANSLATE to bank 61");
    assert_eq!(m.get(0xc00), 62, "firmware S2CR untouched");
    // Every later attach shares the bank.
    assert_eq!(
        smmu.attach_bypass(&mut m, 0x540, 0).unwrap().via,
        Via::Bank(61)
    );
}

#[test]
fn quirk_is_detected_on_an_unused_s2cr_and_put_back() {
    let mut m = Model::new();
    m.bypass_quirk = true;
    m.firmware_smr(0, 0xa0, 0, 3); // UFS: the firmware translates in bank 3
    m.set(Model::gr1(12), 0x0001_0000);
    m.set(Model::cb(3, 0), SCTLR_M);
    let mut smmu = probe(&mut m);
    let a = smmu.attach_bypass(&mut m, 0xa0, 0).unwrap();
    assert!(a.inherited);
    assert_eq!(a.slot, 0);
    // The test write went to slot 1 (unused) and was put back; slot 0 was written once, with the final value.
    let slot1: Vec<u32> = m
        .writes
        .iter()
        .filter(|w| w.0 == 0xc04)
        .map(|w| w.1)
        .collect();
    assert_eq!(slot1, [(1 << 16) | 0xff, 2 << 16]);
    let slot0: Vec<u32> = m
        .writes
        .iter()
        .filter(|w| w.0 == 0xc00)
        .map(|w| w.1)
        .collect();
    assert_eq!(slot0.len(), 1);
    assert_eq!(m.get(0x800), SMR_VALID | 0xa0, "firmware SMR kept");
    assert!(!m.writes.iter().any(|w| w.0 == 0x800));
}

#[test]
fn covered_streams_reuse_the_firmware_smr_and_partial_overlap_is_refused() {
    let mut m = Model::new();
    m.firmware_smr(7, 0x800, 0x2, 1 << 16); // display: 0x800 and 0x802
    let mut smmu = probe(&mut m);
    let a = smmu.attach_bypass(&mut m, 0x802, 0).unwrap();
    assert_eq!((a.slot, a.inherited), (7, true));
    assert_eq!(
        smmu.attach_bypass(&mut m, 0x800, 0x6),
        Err(Error::Overlap {
            slot: 7,
            smr: Smr {
                valid: true,
                id: 0x800,
                mask: 0x2
            }
        })
    );
    // Streams no SMR matches get a free one.
    assert_eq!(smmu.attach_bypass(&mut m, 0x804, 0).unwrap().slot, 0);
}

#[test]
fn two_covering_smrs_are_refused() {
    let mut m = Model::new();
    m.firmware_smr(1, 0xc0, 0x1, 1 << 16);
    m.firmware_smr(2, 0xc0, 0x3, 1 << 16);
    let mut smmu = probe(&mut m);
    assert_eq!(
        smmu.attach_bypass(&mut m, 0xc1, 0),
        Err(Error::MultiMatch {
            first: 1,
            second: 2
        })
    );
}

#[test]
fn a_refused_write_is_an_error() {
    let mut m = Model::new();
    let smmu = probe(&mut m);
    // Firmware that keeps FAULT whatever is written, without the bank fallback being possible: every bank used.
    m.bypass_quirk = true;
    for b in 0..64 {
        m.set(Model::cb(b, 0), SCTLR_M);
    }
    let mut smmu2 = probe(&mut m);
    assert_eq!(smmu2.attach_bypass(&mut m, 0xa0, 0), Err(Error::NoFreeBank));
    // A hypervisor that drops SMR writes.
    let mut m = Model::new();
    let mut smmu3 = probe(&mut m);
    struct DropSmr<'a>(&'a mut Model);
    impl Regs for DropSmr<'_> {
        fn read32(&mut self, o: usize) -> u32 {
            self.0.read32(o)
        }
        fn write32(&mut self, o: usize, v: u32) {
            if !(0x800..0xa00).contains(&o) {
                self.0.write32(o, v)
            }
        }
    }
    assert_eq!(
        smmu3.attach_bypass(&mut DropSmr(&mut m), 0xa0, 0),
        Err(Error::Rejected {
            offset: 0x800,
            wrote: SMR_VALID | 0xa0,
            read: 0
        })
    );
    let _ = smmu.geometry();
}

#[test]
fn no_free_smr() {
    let mut m = Model::new();
    for i in 0..128 {
        m.firmware_smr(i, 0x1000 + i as u16, 0, 1 << 16);
    }
    let mut smmu = probe(&mut m);
    assert_eq!(smmu.attach_bypass(&mut m, 0xa0, 0), Err(Error::NoFreeSmr));
}

#[test]
fn faults_are_read_and_acknowledged() {
    let mut m = Model::new();
    let smmu = probe(&mut m);
    assert_eq!(smmu.global_fault(&mut m), None);
    m.set(0x48, 0x2);
    m.set(0x54, 0xa0);
    let g = smmu.global_fault(&mut m).unwrap();
    assert_eq!(g.gfsr, 0x2);
    assert_eq!(m.get(0x48), 0, "acknowledged");
    assert!(format!("{g}").contains("USF"));

    m.set(Model::cb(5, 0x58), 0x8);
    m.set(Model::cb(5, 0x60), 0x1234_5000);
    m.set(Model::cb(5, 0x64), 0x1);
    m.set(Model::cb(5, 0x68), 1 << 4);
    m.set(Model::gr1(0x400 + 4 * 5), 0xa0);
    let c = smmu.context_fault(&mut m, 5).unwrap();
    assert_eq!((c.fsr, c.far, c.stream), (0x8, 0x1_1234_5000, 0xa0));
    assert_eq!(m.get(Model::cb(5, 0x58)), 0);
    assert!(format!("{c}").contains("write"));
    assert_eq!(smmu.context_fault(&mut m, 5), None);
    assert_eq!(smmu.context_fault(&mut m, 64), None);
}

#[test]
fn extended_ids_and_no_stream_matching_are_refused() {
    let mut m = Model::new();
    m.set(0x000, scr0::EXIDENABLE);
    let mut smmu = probe(&mut m);
    assert_eq!(smmu.attach_bypass(&mut m, 0xa0, 0), Err(Error::ExtendedIds));
    let mut m = Model::new();
    m.set(0x20, ID0 & !(1 << 27));
    let mut smmu = probe(&mut m);
    assert_eq!(
        smmu.attach_bypass(&mut m, 0xa0, 0),
        Err(Error::NoStreamMatching)
    );
}

#[test]
fn report_lists_what_the_firmware_left() {
    let mut m = Model::new();
    m.firmware_smr(3, 0x800, 0x2, 1 << 16);
    m.firmware_smr(4, 0xa0, 0, 9);
    m.set(Model::gr1(4 * 9), 0x0001_ff00);
    m.set(Model::cb(9, 0), 0x1);
    let smmu = probe(&mut m);
    let mut s = String::new();
    smmu.report(&mut s).unwrap();
    assert!(
        s.starts_with("MMU-500 r2p1: 128 SMRs, 64 context banks"),
        "{s}"
    );
    assert!(
        s.contains("SMR   3: id 0x0800 mask 0x0002 -> bypass"),
        "{s}"
    );
    assert!(
        s.contains("SMR   4: id 0x00a0 mask 0x0000 -> context bank 9"),
        "{s}"
    );
    assert!(
        s.contains("context bank   9: CBAR 0x0001ff00 SCTLR 0x00000001"),
        "{s}"
    );
    assert!(s.contains("126 SMRs and 63 context banks unused"), "{s}");
}

#[test]
fn iommus_cells() {
    let cells: Vec<u8> = [0x33u32, 0x800, 0x2, 0x83, 0x0, 0x0, 0x33, 0xa0, 0x0]
        .iter()
        .flat_map(|c| c.to_be_bytes())
        .collect();
    let s: Vec<_> = streams(&cells, 0x33).collect();
    assert_eq!(s, [(0x800, 0x2), (0xa0, 0)]);
}

/// The Galaxy Book Go's UFS as its firmware leaves it (seen on the laptop): SMR 3 for stream 0xa0, context bank 2 of
/// type "stage 1 translation, stage 2 bypass" with stage 1 off. That already lets the stream through, so nothing is
/// written, not even the test of the hypervisor's BYPASS handling.
#[test]
fn a_firmware_mapping_that_is_already_untranslated_stays() {
    for quirk in [false, true] {
        let mut m = Model::new();
        m.bypass_quirk = quirk;
        m.firmware_smr(3, 0xa0, 0, 2);
        m.set(Model::gr1(4 * 2), 0x0001_f000);
        m.set(Model::cb(2, 0), 0x009f_00e0);
        let mut smmu = probe(&mut m);
        let n = m.writes.len();
        let a = smmu.attach_bypass(&mut m, 0xa0, 0).unwrap();
        assert_eq!(
            a,
            Attached {
                slot: 3,
                inherited: true,
                changed: false,
                via: Via::Bank(2)
            }
        );
        assert_eq!(m.writes.len(), n, "{:?}", &m.writes[n..]);
        assert_eq!(m.get(0xc0c), 2);
    }
}

/// A firmware mapping that does translate is still taken over, to a stand-in bank under the hypervisor's quirk.
#[test]
fn a_firmware_mapping_that_translates_is_replaced() {
    for sctlr_and_cbar in [(SCTLR_M, 0x0001_f000), (0, 0x0003_0000)] {
        let mut m = Model::new();
        m.bypass_quirk = true;
        m.firmware_smr(3, 0xa0, 0, 2);
        m.set(Model::cb(2, 0), sctlr_and_cbar.0);
        m.set(Model::gr1(4 * 2), sctlr_and_cbar.1);
        let mut smmu = probe(&mut m);
        let a = smmu.attach_bypass(&mut m, 0xa0, 0).unwrap();
        assert!(a.inherited && a.changed, "{a:?}");
        assert_eq!(a.slot, 3);
        assert!(matches!(a.via, Via::Bank(b) if b != 2), "{a:?}");
    }
}

// The Galaxy Book Go: a stand-in bank faulted above 4 GiB, the firmware's banks do not. The new stream takes USB's bank.
#[test]
fn attach_like_points_at_the_models_bank_and_writes_s2cr_before_smr() {
    let mut m = Model::new();
    m.bypass_quirk = true;
    m.set(Model::gr1(4), 0x0001_f000); // CBAR of bank 1
    m.set(Model::cb(1, 0), 0xe0);
    m.firmware_smr(2, 0x540, 0, 1); // USB, S2CR translate to bank 1
    let mut s = probe(&mut m);
    m.writes.clear();
    let a = s.attach_like(&mut m, 0xc0, 1, 0x540).unwrap();
    assert_eq!(a.via, Via::Bank(1));
    assert!(a.changed && !a.inherited);
    let slot = a.slot;
    assert_ne!(slot, 2);
    assert_eq!(m.writes[0], (0xc00 + 4 * slot, 1));
    assert_eq!(m.writes[1], (0x800 + 4 * slot, SMR_VALID | (1 << 16) | 0xc0));
    assert_eq!(m.writes.len(), 2, "no bank register is written");
}

// A reload after a kernel that used a stand-in: the SMR stays, its S2CR moves to the model's bank.
#[test]
fn attach_like_moves_an_earlier_kernels_stand_in() {
    let mut m = Model::new();
    m.bypass_quirk = true;
    m.set(Model::gr1(4), 0x0001_f000);
    m.set(Model::cb(1, 0), 0xe0);
    m.firmware_smr(2, 0x540, 0, 1);
    m.set(Model::gr1(4 * 60), 0x0001_0000);
    m.firmware_smr(5, 0xc0, 1, 60);
    let mut s = probe(&mut m);
    m.writes.clear();
    let a = s.attach_like(&mut m, 0xc0, 1, 0x540).unwrap();
    assert_eq!((a.slot, a.inherited, a.via), (5, true, Via::Bank(1)));
    assert_eq!(m.writes, vec![(0xc00 + 4 * 5, 1)]);
}

#[test]
fn attach_like_needs_a_model_that_is_untranslated() {
    let mut m = Model::new();
    let mut s = probe(&mut m);
    assert_eq!(s.attach_like(&mut m, 0xc0, 1, 0x540), Err(Error::NoModelStream { like: 0x540 }));
    // A model that translates is no model.
    m.set(Model::gr1(4), 0x0001_f000);
    m.set(Model::cb(1, 0), 0xe1);
    m.firmware_smr(2, 0x540, 0, 1);
    let mut s = probe(&mut m);
    assert_eq!(s.attach_like(&mut m, 0xc0, 1, 0x540), Err(Error::NoModelStream { like: 0x540 }));
}
