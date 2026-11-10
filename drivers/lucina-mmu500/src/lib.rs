//! ARM MMU-500 (SMMU architecture v2) as Qualcomm SoCs integrate it: the apps SMMU at 0x15000000 of SC7180
//! (Galaxy Book Go) and SM7150 (surya). Behaviour follows Linux's `arm-smmu` and `arm-smmu-qcom` drivers (v6.12,
//! notes in `docs/research/gbgo-smmu.md`), with one rule of our own: nothing the firmware set up is reset.
//!
//! * [`Smmu::probe`] only reads: the ID registers ([`Geometry`]), sCR0, every stream match register (SMR) with its
//!   stream-to-context register (S2CR), and each context bank's CBAR and SCTLR. [`Smmu::report`] prints them.
//! * [`Smmu::attach_bypass`] lets one master's streams (`iommus = <&smmu SID MASK>`) through untranslated, so that
//!   its DMA addresses are physical addresses. It reuses an SMR the firmware left that covers them (Linux keeps
//!   such SMRs in `qcom_smmu_cfg_probe` and reuses them in `arm_smmu_find_sme`), refuses an SMR that matches only
//!   some of them, and otherwise takes a free SMR, writing the S2CR before the SMR. Qualcomm's hypervisor may store
//!   an S2CR of type BYPASS as FAULT; then a context bank with stage 1 disabled stands in for bypass, as Linux's
//!   `bypass_quirk` does, but the quirk is detected on an unused S2CR instead of a live one.
//! * [`Smmu::global_fault`] and [`Smmu::context_fault`] read and acknowledge fault records.
//!
//! The hypervisor traps some of these registers, and deployed versions of it mis-emulate stores from the zero
//! register (Linux's `QCOM_DUMMY_VAL`): [`Regs::write32`] must store from a general-purpose register. Global
//! registers (sCR0, sACR, the TLB maintenance registers) are never written here.

#![no_std]
#![forbid(unsafe_code)]

use core::fmt;

/// Register access to the SMMU's window (the `reg` of its device tree node), by byte offset from its start.
pub trait Regs {
    /// One 32-bit load.
    fn read32(&mut self, offset: usize) -> u32;
    /// One 32-bit store, from a general-purpose register: never from `wzr`, even when `value` is 0.
    fn write32(&mut self, offset: usize, value: u32);
}

/// Most stream match register groups handled. Qualcomm's groups beyond 128 do not behave like the architected
/// ones (Linux caps them in `qcom_smmu_cfg_probe`).
pub const MAX_SMRS: usize = 128;
/// Most context banks (ID1.NUMCB is 8 bits).
pub const MAX_BANKS: usize = 256;

/// Global register space 0.
mod gr0 {
    pub const SCR0: usize = 0x000;
    pub const ID0: usize = 0x020;
    pub const ID1: usize = 0x024;
    pub const ID2: usize = 0x028;
    pub const ID7: usize = 0x03c;
    pub const SGFSR: usize = 0x048;
    pub const SGFSYNR0: usize = 0x050;

    pub fn smr(n: usize) -> usize {
        0x800 + 4 * n
    }

    pub fn s2cr(n: usize) -> usize {
        0xc00 + 4 * n
    }
}

/// Global register space 1 (offsets within its page).
mod gr1 {
    pub fn cbar(n: usize) -> usize {
        4 * n
    }
    pub fn cba2r(n: usize) -> usize {
        0x800 + 4 * n
    }

    pub fn cbfrsynra(n: usize) -> usize {
        0x400 + 4 * n
    }
}

/// A context bank's page.
mod cb {
    pub const SCTLR: usize = 0x00;
    pub const FSR: usize = 0x58;
    pub const FAR: usize = 0x60;
    pub const FSYNR0: usize = 0x68;
}

/// sCR0 bits.
pub mod scr0 {
    /// Client port disable: the SMMU lets every transaction through untranslated.
    pub const CLIENTPD: u32 = 1 << 0;
    /// Extended (16-bit) stream IDs; the SMR valid bit then lives in S2CR.EXIDVALID.
    pub const EXIDENABLE: u32 = 1 << 3;
    /// Unidentified stream fault: a stream no SMR matches faults instead of passing through.
    pub const USFCFG: u32 = 1 << 10;
}

const SMR_VALID: u32 = 1 << 31;
const S2CR_EXIDVALID: u32 = 1 << 10;
const S2CR_TYPE_SHIFT: u32 = 16;
const S2CR_CBNDX_MASK: u32 = 0xff;
const S2CR_PRIVCFG_SHIFT: u32 = 24;
/// CBAR.TYPE = 1: stage 1 translation, stage 2 bypass.
const CBAR_TYPE_S1_TRANS_S2_BYPASS: u32 = 1 << 16;
const SCTLR_M: u32 = 1 << 0;
/// The FSR bits that record a fault (Linux's `ARM_SMMU_CB_FSR_FAULT`): MULTI, SS, UUT, ASF, TLBLKF, TLBMCF, EF,
/// PF, AFF, TF.
const FSR_FAULT: u32 = (1 << 31) | (1 << 30) | 0x1fe;

/// What an SMMU can do, from ID0, ID1, ID2 and ID7.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Geometry {
    /// Stream match register groups (ID0.NUMSMRG, at most [`MAX_SMRS`]).
    pub smrs: usize,
    /// Context banks (ID1.NUMCB).
    pub banks: usize,
    /// Of them, the stage-2-only ones (ID1.NUMS2CB).
    pub stage2_only_banks: usize,
    /// 12 (4 KiB register pages) or 16 (64 KiB), ID1.PAGESIZE.
    pub page_shift: u32,
    /// Pages of global register space; context bank 0 is the page after them (`1 << (ID1.NUMPAGENDXB + 1)`).
    pub global_pages: usize,
    /// Whether streams are told apart by stream match registers (ID0.SMS).
    pub stream_matching: bool,
    /// Stream ID bits (ID0.NUMSIDB).
    pub stream_id_bits: u32,
    /// Whether 16-bit extended stream IDs are implemented (ID0.EXIDS).
    pub extended_ids: bool,
    pub stage1: bool,
    pub stage2: bool,
    /// Whether table walks snoop the CPU caches (ID0.CTTW).
    pub coherent_walk: bool,
    /// Input, intermediate and output address bits (ID2.UBS, IAS, OAS).
    pub ubs_bits: u32,
    pub ias_bits: u32,
    pub oas_bits: u32,
    /// MMU-500 revision `r<major>p<minor>` (ID7).
    pub major: u8,
    pub minor: u8,
}

impl Geometry {
    /// Reads the ID registers.
    pub fn read(r: &mut impl Regs) -> Self {
        let id0 = r.read32(gr0::ID0);
        let id1 = r.read32(gr0::ID1);
        let id2 = r.read32(gr0::ID2);
        let id7 = r.read32(gr0::ID7);
        let size = |code: u32| match code {
            0 => 32,
            1 => 36,
            2 => 40,
            3 => 42,
            4 => 44,
            _ => 48,
        };
        Geometry {
            smrs: ((id0 & 0xff) as usize).min(MAX_SMRS),
            banks: (id1 & 0xff) as usize,
            stage2_only_banks: ((id1 >> 16) & 0xff) as usize,
            page_shift: if id1 & (1 << 31) != 0 { 16 } else { 12 },
            global_pages: 1 << (((id1 >> 28) & 7) + 1),
            stream_matching: id0 & (1 << 27) != 0,
            stream_id_bits: (id0 >> 9) & 0xf,
            extended_ids: id0 & (1 << 8) != 0,
            stage1: id0 & (1 << 30) != 0,
            stage2: id0 & (1 << 29) != 0,
            coherent_walk: id0 & (1 << 14) != 0,
            ubs_bits: size((id2 >> 8) & 0xf),
            ias_bits: size(id2 & 0xf),
            oas_bits: size((id2 >> 4) & 0xf),
            major: ((id7 >> 4) & 0xf) as u8,
            minor: (id7 & 0xf) as u8,
        }
    }

    /// The size of the register window the ID registers describe: global space plus as many context bank pages.
    pub fn window_size(&self) -> usize {
        (2 * self.global_pages) << self.page_shift
    }

    fn gr1(&self, offset: usize) -> usize {
        (1 << self.page_shift) + offset
    }

    fn cb(&self, bank: usize, offset: usize) -> usize {
        ((self.global_pages + bank) << self.page_shift) + offset
    }
}

/// A stream match register: the streams whose ID equals `id` in every bit that is clear in `mask`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Smr {
    pub valid: bool,
    pub id: u16,
    pub mask: u16,
}

impl Smr {
    fn decode(smr: u32, s2cr: u32, exid: bool) -> Self {
        if exid {
            Smr {
                valid: s2cr & S2CR_EXIDVALID != 0,
                id: smr as u16,
                mask: (smr >> 16) as u16,
            }
        } else {
            Smr {
                valid: smr & SMR_VALID != 0,
                id: smr as u16,
                mask: ((smr >> 16) & 0x7fff) as u16,
            }
        }
    }

    fn encode(&self, exid: bool) -> u32 {
        let valid = if self.valid && !exid { SMR_VALID } else { 0 };
        valid | (u32::from(self.mask) << 16) | u32::from(self.id)
    }

    /// Whether this SMR matches stream `sid`.
    pub fn matches(&self, sid: u16) -> bool {
        self.valid && (sid ^ self.id) & !self.mask == 0
    }
}

/// What an S2CR does with the streams its SMR matches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum S2crType {
    /// Translate in context bank `cbndx`.
    Translate,
    Bypass,
    Fault,
    Reserved,
}

/// A stream-to-context register.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct S2cr {
    pub typ: S2crType,
    pub cbndx: u8,
    pub privcfg: u8,
}

impl S2cr {
    const BYPASS: S2cr = S2cr {
        typ: S2crType::Bypass,
        cbndx: 0xff,
        privcfg: 0,
    };

    fn decode(raw: u32) -> Self {
        S2cr {
            typ: match (raw >> S2CR_TYPE_SHIFT) & 3 {
                0 => S2crType::Translate,
                1 => S2crType::Bypass,
                2 => S2crType::Fault,
                _ => S2crType::Reserved,
            },
            cbndx: (raw & S2CR_CBNDX_MASK) as u8,
            privcfg: ((raw >> S2CR_PRIVCFG_SHIFT) & 3) as u8,
        }
    }

    fn encode(&self, exid_valid: bool) -> u32 {
        let typ = match self.typ {
            S2crType::Translate => 0,
            S2crType::Bypass => 1,
            S2crType::Fault => 2,
            S2crType::Reserved => 3,
        };
        (u32::from(self.privcfg) << S2CR_PRIVCFG_SHIFT)
            | (typ << S2CR_TYPE_SHIFT)
            | if exid_valid { S2CR_EXIDVALID } else { 0 }
            | u32::from(self.cbndx)
    }
}

/// The two registers of a context bank that tell whether someone uses it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Bank {
    pub cbar: u32,
    pub sctlr: u32,
}

impl Bank {
    /// Programmed by the firmware (or the hypervisor), as far as the registers tell.
    pub fn in_use(&self) -> bool {
        self.cbar != 0 || self.sctlr & SCTLR_M != 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// No SMR lets this stream through untranslated.
    NoModelStream { like: u16 },
    /// The ID registers describe a register window larger than the device tree's `reg`.
    Window { needed: usize, size: usize },
    /// The SMMU has no stream matching (ID0.SMS clear), so streams cannot be told apart.
    NoStreamMatching,
    /// The SMMU uses extended stream IDs (sCR0.EXIDENABLE), which this driver does not program.
    ExtendedIds,
    /// The SMR in `slot` matches some but not all of the requested streams.
    Overlap { slot: usize, smr: Smr },
    /// Two SMRs cover the requested streams: a stream would match both.
    MultiMatch { first: usize, second: usize },
    /// Every SMR is in use.
    NoFreeSmr,
    /// No context bank is free to stand in for bypass.
    NoFreeBank,
    /// A register read back differently from what was written: the hypervisor refused or changed it.
    Rejected {
        offset: usize,
        wrote: u32,
        read: u32,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Error::NoModelStream { like } => {
                write!(f, "no SMR lets stream {like:#x} through untranslated")
            }
            Error::Window { needed, size } => {
                write!(
                    f,
                    "register window is {size:#x} bytes, the ID registers need {needed:#x}"
                )
            }
            Error::NoStreamMatching => f.write_str("no stream matching (ID0.SMS = 0)"),
            Error::ExtendedIds => f.write_str("extended stream IDs are enabled (sCR0.EXIDENABLE)"),
            Error::Overlap { slot, smr } => write!(
                f,
                "SMR {slot} (id {:#x}, mask {:#x}) matches only part of the streams",
                smr.id, smr.mask
            ),
            Error::MultiMatch { first, second } => {
                write!(f, "SMRs {first} and {second} both cover the streams")
            }
            Error::NoFreeSmr => f.write_str("no free stream match register"),
            Error::NoFreeBank => f.write_str("no free context bank"),
            Error::Rejected {
                offset,
                wrote,
                read,
            } => write!(
                f,
                "register +{offset:#x}: wrote {wrote:#x}, reads {read:#x}"
            ),
        }
    }
}

/// How [`Smmu::attach_bypass`] let the streams through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Via {
    /// S2CR type BYPASS.
    Bypass,
    /// S2CR type TRANSLATE to this context bank, which has stage 1 disabled (the hypervisor turns BYPASS into
    /// FAULT).
    Bank(u8),
}

/// The result of [`Smmu::attach_bypass`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Attached {
    /// The SMR (and S2CR) now used for the streams.
    pub slot: usize,
    /// Whether the SMR was the firmware's (left as it was) rather than a free one.
    pub inherited: bool,
    /// Whether the stream's S2CR was written. False when the firmware already let the stream through untranslated.
    pub changed: bool,
    pub via: Via,
}

/// A global fault record (sGFSR and sGFSYNR0..2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GlobalFault {
    pub gfsr: u32,
    pub gfsynr: [u32; 3],
}

impl fmt::Display for GlobalFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "global fault: sGFSR {:#x}", self.gfsr)?;
        for (bit, name) in [
            (1, "USF (unidentified stream)"),
            (0, "ICF"),
            (2, "SMCF (multi-match)"),
        ] {
            if self.gfsr & (1 << bit) != 0 {
                write!(f, " {name}")?;
            }
        }
        write!(
            f,
            ", sGFSYNR {:#x} {:#x} {:#x} (stream {:#x})",
            self.gfsynr[0],
            self.gfsynr[1],
            self.gfsynr[2],
            self.gfsynr[1] & 0xffff
        )
    }
}

/// A context bank fault record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContextFault {
    pub bank: usize,
    pub fsr: u32,
    pub far: u64,
    pub fsynr0: u32,
    /// The faulting stream (CBFRSYNRA).
    pub stream: u16,
}

impl fmt::Display for ContextFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "context bank {} fault: FSR {:#x}, address {:#x}, FSYNR0 {:#x} ({}), stream {:#x}",
            self.bank,
            self.fsr,
            self.far,
            self.fsynr0,
            if self.fsynr0 & (1 << 4) != 0 {
                "write"
            } else {
                "read"
            },
            self.stream
        )
    }
}

/// An MMU-500 as the firmware left it, plus what this driver changed.
pub struct Smmu {
    geometry: Geometry,
    scr0: u32,
    smrs: [Smr; MAX_SMRS],
    s2crs: [S2cr; MAX_SMRS],
    banks: [Bank; MAX_BANKS],
    /// SMR slots this driver programmed.
    claimed: [bool; MAX_SMRS],
    bypass_bank: Option<u8>,
    /// Whether the hypervisor stores an S2CR BYPASS as FAULT; unknown until the first attach.
    quirk: Option<bool>,
}

impl Smmu {
    /// Reads the SMMU's state; writes nothing. `window` is the size of its register window.
    pub fn probe(r: &mut impl Regs, window: usize) -> Result<Smmu, Error> {
        let geometry = Geometry::read(r);
        let needed = geometry.window_size();
        if needed > window || geometry.banks > MAX_BANKS {
            return Err(Error::Window {
                needed,
                size: window,
            });
        }
        let mut smmu = Smmu {
            geometry,
            scr0: r.read32(gr0::SCR0),
            smrs: [Smr::default(); MAX_SMRS],
            s2crs: [S2cr::BYPASS; MAX_SMRS],
            banks: [Bank::default(); MAX_BANKS],
            claimed: [false; MAX_SMRS],
            bypass_bank: None,
            quirk: None,
        };
        let exid = smmu.exid();
        for i in 0..geometry.smrs {
            let s2cr = r.read32(gr0::s2cr(i));
            smmu.smrs[i] = Smr::decode(r.read32(gr0::smr(i)), s2cr, exid);
            smmu.s2crs[i] = S2cr::decode(s2cr);
        }
        for i in 0..geometry.banks {
            smmu.banks[i] = Bank {
                cbar: r.read32(geometry.gr1(gr1::cbar(i))),
                sctlr: r.read32(geometry.cb(i, cb::SCTLR)),
            };
        }
        Ok(smmu)
    }

    pub fn geometry(&self) -> &Geometry {
        &self.geometry
    }

    pub fn scr0(&self) -> u32 {
        self.scr0
    }

    /// The SMRs and S2CRs, as read at probe time and changed since.
    pub fn smrs(&self) -> impl Iterator<Item = (usize, Smr, S2cr)> + '_ {
        (0..self.geometry.smrs).map(|i| (i, self.smrs[i], self.s2crs[i]))
    }

    pub fn banks(&self) -> &[Bank] {
        &self.banks[..self.geometry.banks]
    }

    fn exid(&self) -> bool {
        self.scr0 & scr0::EXIDENABLE != 0
    }

    /// Lets the streams `sid` (with the don't-care bits `mask`, as in `iommus = <&smmu SID MASK>`) through
    /// untranslated.
    pub fn attach_bypass(
        &mut self,
        r: &mut impl Regs,
        sid: u16,
        mask: u16,
    ) -> Result<Attached, Error> {
        if !self.geometry.stream_matching {
            return Err(Error::NoStreamMatching);
        }
        if self.exid() {
            return Err(Error::ExtendedIds);
        }
        let mut covering = None;
        for (i, smr) in self.smrs[..self.geometry.smrs].iter().enumerate() {
            if !smr.valid || (sid ^ smr.id) & !(mask | smr.mask) != 0 {
                continue; // no stream matches both
            }
            if mask & !smr.mask != 0 {
                return Err(Error::Overlap { slot: i, smr: *smr });
            }
            if let Some(first) = covering {
                return Err(Error::MultiMatch { first, second: i });
            }
            covering = Some(i);
        }
        let slot = match covering {
            Some(i) => i,
            None => self.free_smr(None).ok_or(Error::NoFreeSmr)?,
        };

        // The firmware may already let the stream through: its own S2CR BYPASS, or (the Galaxy Book Go's UFS, SMR 3)
        // a context bank with stage 1 off. Then nothing is written: that mapping is known to work, and its SCTLR
        // sets the attributes of the stream's memory accesses, which a stand-in bank would change.
        if covering.is_some()
            && let Some(via) = self.untranslated(r, slot)
        {
            self.claimed[slot] = true;
            return Ok(Attached {
                slot,
                inherited: true,
                changed: false,
                via,
            });
        }

        let s2cr = self.bypass_s2cr(r, slot)?;
        self.write_checked(r, gr0::s2cr(slot), s2cr.encode(false), 0x0003_00ff)?;
        self.s2crs[slot] = s2cr;
        if covering.is_none() {
            let smr = Smr {
                valid: true,
                id: sid,
                mask,
            };
            self.write_checked(r, gr0::smr(slot), smr.encode(false), u32::MAX)?;
            self.smrs[slot] = smr;
        }
        self.claimed[slot] = true;
        Ok(Attached {
            slot,
            inherited: covering.is_some(),
            changed: true,
            via: match s2cr.typ {
                S2crType::Translate => Via::Bank(s2cr.cbndx),
                _ => Via::Bypass,
            },
        })
    }

    /// How the S2CR of `slot` lets streams through untranslated as it stands, if it does: BYPASS, or TRANSLATE to a
    /// bank of type "stage 1 translation, stage 2 bypass" whose SCTLR.M is off.
    fn untranslated(&self, r: &mut impl Regs, slot: usize) -> Option<Via> {
        let s2cr = S2cr::decode(r.read32(gr0::s2cr(slot)));
        match s2cr.typ {
            S2crType::Bypass => Some(Via::Bypass),
            S2crType::Translate => {
                let bank = usize::from(s2cr.cbndx);
                if bank >= self.geometry.banks {
                    return None;
                }
                let cbar = r.read32(self.geometry.gr1(gr1::cbar(bank)));
                let sctlr = r.read32(self.geometry.cb(bank, cb::SCTLR));
                (cbar & 0x0003_0000 == CBAR_TYPE_S1_TRANS_S2_BYPASS && sctlr & SCTLR_M == 0)
                    .then_some(Via::Bank(s2cr.cbndx))
            }
            _ => None,
        }
    }

    /// A slot whose SMR matches nothing and that this driver has not taken, other than `except`.
    fn free_smr(&self, except: Option<usize>) -> Option<usize> {
        (0..self.geometry.smrs)
            .find(|&i| !self.smrs[i].valid && !self.claimed[i] && Some(i) != except)
    }

    /// The S2CR that lets streams through: BYPASS, or a translation to the stand-in bank when the hypervisor
    /// turns BYPASS into FAULT. The first call finds out which, on an S2CR whose SMR matches nothing if there
    /// is one (then put back as it was), else on `slot` itself.
    fn bypass_s2cr(&mut self, r: &mut impl Regs, slot: usize) -> Result<S2cr, Error> {
        let quirk = match self.quirk {
            Some(q) => q,
            None => {
                let test = if self.smrs[slot].valid {
                    self.free_smr(Some(slot))
                } else {
                    Some(slot)
                };
                let test = test.unwrap_or(slot);
                let old = r.read32(gr0::s2cr(test));
                r.write32(gr0::s2cr(test), S2cr::BYPASS.encode(false));
                let q = S2cr::decode(r.read32(gr0::s2cr(test))).typ != S2crType::Bypass;
                if test != slot {
                    r.write32(gr0::s2cr(test), old);
                }
                self.quirk = Some(q);
                q
            }
        };
        if !quirk {
            return Ok(S2cr::BYPASS);
        }
        let bank = match self.bypass_bank {
            Some(b) => b,
            None => {
                let b = self.free_bank().ok_or(Error::NoFreeBank)?;
                // As Linux's qcom_smmu_cfg_probe: stage 1 off, then stage 1 translation / stage 2 bypass type.
                r.write32(self.geometry.cb(b, cb::SCTLR), 0);
                self.write_checked(
                    r,
                    self.geometry.gr1(gr1::cbar(b)),
                    CBAR_TYPE_S1_TRANS_S2_BYPASS,
                    0x0003_0000,
                )?;
                self.banks[b] = Bank {
                    cbar: CBAR_TYPE_S1_TRANS_S2_BYPASS,
                    sctlr: 0,
                };
                self.bypass_bank = Some(b as u8);
                b as u8
            }
        };
        Ok(S2cr {
            typ: S2crType::Translate,
            cbndx: bank,
            privcfg: 0,
        })
    }

    /// The highest context bank that the registers show unused and no S2CR points at, above the stage-2-only ones
    /// (which are the lowest-numbered).
    fn free_bank(&self) -> Option<usize> {
        let referenced = |b: usize| {
            self.smrs[..self.geometry.smrs]
                .iter()
                .zip(&self.s2crs)
                .any(|(smr, s2cr)| {
                    smr.valid && s2cr.typ == S2crType::Translate && usize::from(s2cr.cbndx) == b
                })
        };
        let first = self.geometry.stage2_only_banks.min(self.geometry.banks);
        (first..self.geometry.banks)
            .rev()
            .find(|&b| !self.banks[b].in_use() && !referenced(b))
    }

    fn write_checked(
        &self,
        r: &mut impl Regs,
        offset: usize,
        value: u32,
        check: u32,
    ) -> Result<(), Error> {
        r.write32(offset, value);
        let read = r.read32(offset);
        if read & check == value & check {
            Ok(())
        } else {
            Err(Error::Rejected {
                offset,
                wrote: value,
                read,
            })
        }
    }

    /// Lets the streams `sid`/`mask` through the way the firmware lets `like` through: the same S2CR, whichever bank that
    /// names. For a master whose addresses reach where a working master's do: on the Galaxy Book Go a stand-in bank
    /// (`attach_bypass`) took a level 1 translation fault on an address above 4 GiB, while the firmware's banks carry USB
    /// and UFS DMA there. An SMR already covering the streams, as an earlier kernel's stand-in, is pointed there too.
    pub fn attach_like(
        &mut self,
        r: &mut impl Regs,
        sid: u16,
        mask: u16,
        like: u16,
    ) -> Result<Attached, Error> {
        if !self.geometry.stream_matching {
            return Err(Error::NoStreamMatching);
        }
        if self.exid() {
            return Err(Error::ExtendedIds);
        }
        let (model, _) = (0..self.geometry.smrs)
            .filter(|&i| self.smrs[i].valid && self.smrs[i].matches(like))
            .map(|i| (i, self.untranslated(r, i)))
            .find(|(_, via)| via.is_some())
            .ok_or(Error::NoModelStream { like })?;
        let via = self.untranslated(r, model).ok_or(Error::NoModelStream { like })?;
        let target = S2cr::decode(r.read32(gr0::s2cr(model)));
        let mut covering = None;
        for (i, smr) in self.smrs[..self.geometry.smrs].iter().enumerate() {
            if !smr.valid || (sid ^ smr.id) & !(mask | smr.mask) != 0 {
                continue;
            }
            if mask & !smr.mask != 0 {
                return Err(Error::Overlap { slot: i, smr: *smr });
            }
            if let Some(first) = covering {
                return Err(Error::MultiMatch { first, second: i });
            }
            covering = Some(i);
        }
        let slot = match covering {
            Some(i) => i,
            None => self.free_smr(None).ok_or(Error::NoFreeSmr)?,
        };
        self.write_checked(r, gr0::s2cr(slot), target.encode(false), 0x0003_00ff)?;
        self.s2crs[slot] = target;
        if covering.is_none() {
            let smr = Smr {
                valid: true,
                id: sid,
                mask,
            };
            self.write_checked(r, gr0::smr(slot), smr.encode(false), u32::MAX)?;
            self.smrs[slot] = smr;
        }
        self.claimed[slot] = true;
        Ok(Attached {
            slot,
            inherited: covering.is_some(),
            changed: true,
            via,
        })
    }

    /// CBAR, CBA2R and SCTLR of a context bank, for a log.
    pub fn bank_registers(&self, r: &mut impl Regs, bank: usize) -> Option<(u32, u32, u32)> {
        (bank < self.geometry.banks).then(|| {
            (
                r.read32(self.geometry.gr1(gr1::cbar(bank))),
                r.read32(self.geometry.gr1(gr1::cba2r(bank))),
                r.read32(self.geometry.cb(bank, cb::SCTLR)),
            )
        })
    }

    /// Reads and acknowledges a global fault, if one is recorded.
    pub fn global_fault(&self, r: &mut impl Regs) -> Option<GlobalFault> {
        let gfsr = r.read32(gr0::SGFSR);
        if gfsr == 0 {
            return None;
        }
        let gfsynr = [0, 1, 2].map(|i| r.read32(gr0::SGFSYNR0 + 4 * i));
        r.write32(gr0::SGFSR, gfsr);
        Some(GlobalFault { gfsr, gfsynr })
    }

    /// Reads and acknowledges a fault of context bank `bank`, if one is recorded.
    pub fn context_fault(&self, r: &mut impl Regs, bank: usize) -> Option<ContextFault> {
        if bank >= self.geometry.banks {
            return None;
        }
        let fsr = r.read32(self.geometry.cb(bank, cb::FSR));
        if fsr & FSR_FAULT == 0 {
            return None;
        }
        let far = u64::from(r.read32(self.geometry.cb(bank, cb::FAR)))
            | (u64::from(r.read32(self.geometry.cb(bank, cb::FAR + 4))) << 32);
        let fsynr0 = r.read32(self.geometry.cb(bank, cb::FSYNR0));
        let stream = r.read32(self.geometry.gr1(gr1::cbfrsynra(bank))) as u16;
        r.write32(self.geometry.cb(bank, cb::FSR), fsr);
        Some(ContextFault {
            bank,
            fsr,
            far,
            fsynr0,
            stream,
        })
    }

    /// Prints and acknowledges the recorded faults: the global one and those of the banks a stream is mapped to.
    pub fn report_faults(&self, r: &mut impl Regs, out: &mut impl fmt::Write) -> fmt::Result {
        match self.global_fault(r) {
            Some(f) => writeln!(out, "{f}")?,
            None => writeln!(out, "no global fault")?,
        }
        for (_, smr, s2cr) in self.smrs() {
            if !smr.valid || s2cr.typ != S2crType::Translate {
                continue;
            }
            let bank = usize::from(s2cr.cbndx);
            match self.context_fault(r, bank) {
                Some(f) => writeln!(
                    out,
                    "bank {}: FSR {:#x} FAR {:#x} FSYNR0 {:#x} stream {:#x}",
                    f.bank, f.fsr, f.far, f.fsynr0, f.stream
                )?,
                None => writeln!(out, "bank {bank}: no fault")?,
            }
        }
        Ok(())
    }

    /// Prints the state: geometry, sCR0, the valid SMRs with their S2CRs, the context banks in use.
    pub fn report(&self, out: &mut impl fmt::Write) -> fmt::Result {
        let g = &self.geometry;
        writeln!(
            out,
            "MMU-500 r{}p{}: {} SMRs, {} context banks ({} stage-2 only), {} KiB register pages, \
             {} stream ID bits, stage 1 {}, coherent walk {}, UBS/IAS/OAS {}/{}/{} bits",
            g.major,
            g.minor,
            g.smrs,
            g.banks,
            g.stage2_only_banks,
            1 << (g.page_shift - 10),
            g.stream_id_bits,
            yes(g.stage1),
            yes(g.coherent_walk),
            g.ubs_bits,
            g.ias_bits,
            g.oas_bits
        )?;
        let s = self.scr0;
        writeln!(
            out,
            "sCR0 {s:#x}: clients {}, unmatched streams {}{}",
            if s & scr0::CLIENTPD != 0 {
                "bypass the SMMU (CLIENTPD)"
            } else {
                "translated"
            },
            if s & scr0::USFCFG != 0 {
                "fault"
            } else {
                "bypass"
            },
            if s & scr0::EXIDENABLE != 0 {
                ", extended stream IDs"
            } else {
                ""
            }
        )?;
        let mut free = 0;
        for (i, smr, s2cr) in self.smrs() {
            if !smr.valid {
                free += 1;
                continue;
            }
            write!(
                out,
                "SMR {i:3}: id {:#06x} mask {:#06x} -> ",
                smr.id, smr.mask
            )?;
            match s2cr.typ {
                S2crType::Translate => writeln!(out, "context bank {}", s2cr.cbndx)?,
                S2crType::Bypass => writeln!(out, "bypass")?,
                S2crType::Fault => writeln!(out, "fault")?,
                S2crType::Reserved => writeln!(out, "reserved type")?,
            }
        }
        let mut free_banks = 0;
        for (i, bank) in self.banks().iter().enumerate() {
            if bank.in_use() {
                writeln!(
                    out,
                    "context bank {i:3}: CBAR {:#010x} SCTLR {:#010x}",
                    bank.cbar, bank.sctlr
                )?;
            } else {
                free_banks += 1;
            }
        }
        writeln!(out, "{free} SMRs and {free_banks} context banks unused")
    }
}

fn yes(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

/// The streams of an `iommus` property (big-endian cells) that belong to the SMMU `phandle` with two
/// `#iommu-cells` (stream ID, mask). Entries for other IOMMUs are assumed to have as many cells and are skipped.
pub fn streams(iommus: &[u8], phandle: u32) -> impl Iterator<Item = (u16, u16)> + '_ {
    iommus.as_chunks::<12>().0.iter().filter_map(move |entry| {
        let cell =
            |i: usize| u32::from_be_bytes([entry[i], entry[i + 1], entry[i + 2], entry[i + 3]]);
        (cell(0) == phandle).then(|| (cell(4) as u16, cell(8) as u16))
    })
}

#[cfg(test)]
mod tests;
