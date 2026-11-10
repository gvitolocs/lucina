// Verus MODEL (not a proof of the shipped kernel; project rule D-011): AArch64 VMSAv8-64 stage-1 descriptors
// exactly as the Asterinas v0.19.0 AArch64 port encodes them.
//
// Mirrors (third_party/asterinas/ostd/src/arch/arm/mm/mod.rs), bodies kept token-close except where noted:
//   PteFlags consts                      -> the `pub const` block below (bit positions identical)
//   parse_flags!                         -> macro `parse_flags!` (bit numbers instead of `.bits().ilog2()`)
//   PageTableEntry::{is_present, is_last, paddr, prop, pt_flags, new_page, new_pt}  -> same names below
//   PteTrait::{from_repr, to_repr}       -> `from_repr`, `to_repr` (enum `Repr` = PteScalar)
// Deviations from the source (all semantics-preserving, checked by the ensures clauses):
//   - usize is u64 (AArch64); PagingLevel is u8; PageProperty/PageFlags/PrivilegedPageFlags are plain u8 bit sets
//     with the same bit values as ostd/src/mm/page_prop.rs; CachePolicy is `uncacheable: bool`
//     (Writeback=false, Uncacheable=true; other policies make the real code panic and are out of the model);
//   - new_page is written as a chain of `let fN = ...` instead of `flags |= ...`, so each step can be named in proofs;
//   - debug_assert_eq! is a `requires`.
// Levels use the ASTERINAS convention: 1 = leaf table (4 KiB pages) ... 4 = root. ARM ARM level = 4 - level
// (Asterinas 4 = ARM level 0, 3 = ARM level 1 (1 GiB), 2 = ARM level 2 (2 MiB), 1 = ARM level 3 (4 KiB)).
//
// Configuration (third_party/asterinas/ostd/src/arch/arm/boot/bsp_boot.S): TCR_EL1 = 4 KiB granule for TTBR0 and
// TTBR1, T0SZ = T1SZ = 16 (48-bit VA, walk starts at ARM level 0), inner-shareable, WBWA table walks, IPS = 5
// (48-bit PA); no HA/HD bits (no hardware AF/dirty update); MAIR_EL1 Attr0 = 0xFF (Normal WB), Attr1 = 0x00 (Device-nGnRnE).
//
// ARM ARM (DDI 0487) references are from the author's memory of "D8 The AArch64 Virtual Memory System Architecture"
// (numbering differs between revisions; field positions are what matter). Anything unsure is marked UNSURE.
//   D8.3   VMSAv8-64 translation table format descriptors (bit[0] valid, bit[1] table/page vs block, bits[47:12]
//          output address / next-level table address, bits[63:59] table attributes, bits[58:55] software use)
//   D8.3.2 Stage 1 attribute fields in VMSAv8-64 block and page descriptors: AttrIndx[4:2], NS[5], AP[7:6]
//          (AP[2]=bit7 read-only, AP[1]=bit6 EL0 access), SH[9:8] (0b11 inner shareable), AF[10], nG[11],
//          GP[50] (UNSURE, FEAT_BTI), DBM[51], Contiguous[52], PXN[53], UXN[54] (=XN), software bits [58:55].
//   D8.2   Level 0 block descriptors are reserved with a 4 KiB granule; level 3 descriptor 0b01 is reserved.
//   D13.2  TCR_EL1 (T0SZ[5:0], IRGN0[9:8], ORGN0[11:10], SH0[13:12], TG0[15:14], T1SZ[21:16], IRGN1[25:24],
//          ORGN1[27:26], SH1[29:28], TG1[31:30] (0b10 = 4 KiB), IPS[34:32] (0b101 = 48 bit)).

use vstd::prelude::*;

macro_rules! parse_flags {
    ($val:expr, $from:literal, $to:literal) => {
        (((($val) as u64) >> $from) & 1) << $to
    };
}

verus! {

// ---------------------------------------------------------------- PteFlags (usize bits in the source)
pub const VALID: u64 = 0x1;  // 1 << 0
pub const NON_HUGE: u64 = 0x2;  // 1 << 1
pub const ATTR_DEVICE: u64 = 0x4;  // 1 << 2 (AttrIndx = 1, MAIR Attr1 = Device-nGnRnE)
pub const USER: u64 = 0x40;  // 1 << 6   AP[1]
pub const NO_WRITE: u64 = 0x80;  // 1 << 7   AP[2]
pub const SH_INNER: u64 = 0x300;  // 3 << 8   SH = inner shareable
pub const ACCESSED: u64 = 0x400;  // 1 << 10  AF
pub const NON_GLOBAL: u64 = 0x800;  // 1 << 11  nG
pub const DIRTY: u64 = 0x0008_0000_0000_0000;  // 1 << 51  DBM (software use here)
pub const NO_EXECUTE_KERN: u64 = 0x0020_0000_0000_0000;  // 1 << 53  PXN
pub const NO_EXECUTE_USER: u64 = 0x0040_0000_0000_0000;  // 1 << 54  UXN
pub const HIGH_IGN1: u64 = 0x0080_0000_0000_0000;  // 1 << 55  software
pub const HIGH_IGN2: u64 = 0x0100_0000_0000_0000;  // 1 << 56  software
pub const PHYS_ADDR_MASK: u64 = 0x0000_FFFF_FFFF_F000;

// PageFlags / PrivilegedPageFlags / PageTableFlags bit values (ostd/src/mm/page_prop.rs)
// PageFlags: R=1 W=2 X=4 ACCESSED=8 DIRTY=0x10 AVAIL2=0x80. Privileged: USER=1 GLOBAL=2 AVAIL1=0x40.
// PageTableFlags: AVAIL1=0x40 AVAIL2=0x80.

pub struct PageProperty {
    pub flags: u8,
    pub uncacheable: bool,
    pub priv_flags: u8,
}

/// PteScalar in the source: the architecture-neutral view of an entry.
pub enum Repr {
    Absent,
    PageTable(u64, u8),
    Mapped(u64, PageProperty),
}

/// The 4 KiB granule block/page size at an Asterinas level (1 -> 4 KiB, 2 -> 2 MiB, 3 -> 1 GiB, 4 -> 512 GiB).
pub open spec fn block_size(l: u8) -> u64 {
    if l == 1 {
        0x1000
    } else if l == 2 {
        0x20_0000
    } else if l == 3 {
        0x4000_0000
    } else {
        0x80_0000_0000
    }
}

// ---------------------------------------------------------------- hardware view (ARM ARM)

pub enum Kind {
    Invalid,
    Table,
    Block,
    Page,
}

/// How the MMU classifies descriptor `d` read at Asterinas level `l` (1..=4). D8.3.
pub open spec fn kind(d: u64, l: u8) -> Kind {
    if d & 1 == 0 {
        Kind::Invalid
    } else if l == 1 {
        if d & 2 != 0 {
            Kind::Page
        } else {
            Kind::Invalid  // 0b01 at ARM level 3 is reserved

        }
    } else if d & 2 != 0 {
        Kind::Table
    } else if l == 4 {
        Kind::Invalid  // block at ARM level 0 is reserved for a 4 KiB granule

    } else {
        Kind::Block
    }
}

/// Output address of a block/page: bits [47:12] with the bits below the block size ignored (RES0 for blocks).
pub open spec fn out_addr(d: u64, l: u8) -> u64 {
    d & PHYS_ADDR_MASK & !((block_size(l) - 1) as u64)
}

/// Next-level table address of a table descriptor: bits [47:12].
pub open spec fn table_addr(d: u64) -> u64 {
    d & PHYS_ADDR_MASK
}

pub open spec fn attr_indx(d: u64) -> u64 {
    (d >> 2) & 7
}

pub open spec fn ap(d: u64) -> u64 {
    (d >> 6) & 3
}

pub open spec fn sh(d: u64) -> u64 {
    (d >> 8) & 3
}

pub open spec fn af(d: u64) -> bool {
    d & 0x400 != 0
}

pub open spec fn ng(d: u64) -> bool {
    d & 0x800 != 0
}

pub open spec fn pxn(d: u64) -> bool {
    d & 0x0020_0000_0000_0000 != 0
}

pub open spec fn uxn(d: u64) -> bool {
    d & 0x0040_0000_0000_0000 != 0
}

/// Table attributes NSTable, APTable[1:0], UXNTable, PXNTable (bits 63:59): all zero = the table restricts nothing.
pub open spec fn table_attrs(d: u64) -> u64 {
    d >> 59
}

// ---------------------------------------------------------------- Asterinas decode, as specs
pub open spec fn s_is_present(d: u64) -> bool {
    if d & VALID != 0 {
        true
    } else if d & SH_INNER != 0 {
        true
    } else {
        false
    }
}

pub open spec fn s_is_last(d: u64, level: u8) -> bool {
    level == 1 || d & NON_HUGE == 0
}

pub open spec fn s_paddr(d: u64) -> u64 {
    d & PHYS_ADDR_MASK
}

pub open spec fn s_flags(d: u64) -> u8 {
    let base = (d & 1) | (((!d >> 7) & 1) << 1) | (((d >> 10) & 1) << 3) | (((d >> 51) & 1) << 4)
        | (((d >> 56) & 1) << 7);
    let x = if d & USER != 0 {
        ((!d >> 54) & 1) << 2
    } else {
        ((!d >> 53) & 1) << 2
    };
    (base | x) as u8
}

pub open spec fn s_priv(d: u64) -> u8 {
    (((d >> 6) & 1) | (((!d >> 11) & 1) << 1) | (((d >> 55) & 1) << 6)) as u8
}

pub open spec fn s_cache(d: u64) -> bool {
    d & ATTR_DEVICE != 0
}

pub open spec fn s_pt_flags(d: u64) -> u8 {
    ((((d >> 55) & 1) << 6) | (((d >> 56) & 1) << 7)) as u8
}

pub open spec fn s_to_repr(d: u64, level: u8) -> Repr {
    if !s_is_present(d) {
        Repr::Absent
    } else if s_is_last(d, level) {
        Repr::Mapped(
            s_paddr(d),
            PageProperty { flags: s_flags(d), uncacheable: s_cache(d), priv_flags: s_priv(d) },
        )
    } else {
        Repr::PageTable(s_paddr(d), s_pt_flags(d))
    }
}

// ---------------------------------------------------------------- Asterinas encode, as specs
pub open spec fn s_new_page(paddr: u64, level: u8, flags: u8, priv_flags: u8, unc: bool) -> u64 {
    let f0 = ACCESSED;
    let f1 = if level == 1 {
        f0 | NON_HUGE
    } else {
        f0
    };
    let f2 = f1 | (((flags as u64 >> 0) & 1) << 0) | ((((!flags) as u64 >> 1) & 1) << 7) | (((flags as u64 >> 3)
        & 1) << 10) | (((flags as u64 >> 4) & 1) << 51) | (((priv_flags as u64 >> 0) & 1) << 6) | ((((!priv_flags) as u64
        >> 1) & 1) << 11) | (((priv_flags as u64 >> 6) & 1) << 55) | (((flags as u64 >> 7) & 1)
        << 56);
    let f3 = if priv_flags & 1 != 0 {
        f2 | NO_EXECUTE_KERN | ((((!flags) as u64 >> 2) & 1) << 54)
    } else {
        f2 | NO_EXECUTE_USER | ((((!flags) as u64 >> 2) & 1) << 53)
    };
    let f4 = f3 | SH_INNER;
    let f5 = if unc {
        f4 | ATTR_DEVICE
    } else {
        f4
    };
    paddr | f5
}

pub open spec fn s_new_pt(paddr: u64, flags: u8) -> u64 {
    let f = VALID | NON_HUGE | (((flags as u64 >> 6) & 1) << 55) | (((flags as u64 >> 7) & 1) << 56);
    paddr | f
}

pub open spec fn s_from_repr(r: Repr, level: u8) -> u64 {
    match r {
        Repr::Absent => 0,
        Repr::PageTable(p, f) => s_new_pt(p, f),
        Repr::Mapped(p, prop) => s_new_page(p, level, prop.flags, prop.priv_flags, prop.uncacheable),
    }
}

// ---------------------------------------------------------------- exec: PageTableEntry
pub struct PageTableEntry(pub u64);

impl PageTableEntry {
    fn is_present(&self) -> (r: bool)
        ensures
            r == s_is_present(self.0),
    {
        if self.0 & VALID != 0 {
            // Child page tables and readable pages.
            true
        } else if self.0 & SH_INNER != 0 {
            // Non-readable pages (`new_page()` always sets `SH_INNER`).
            true
        } else {
            // Nothing.
            false
        }
    }

    fn is_last(&self, level: u8) -> (r: bool)
        ensures
            r == s_is_last(self.0, level),
    {
        level == 1 || self.0 & NON_HUGE == 0
    }

    fn paddr(&self) -> (r: u64)
        ensures
            r == s_paddr(self.0),
    {
        self.0 & PHYS_ADDR_MASK
    }

    fn prop(&self) -> (r: PageProperty)
        ensures
            r.flags == s_flags(self.0),
            r.priv_flags == s_priv(self.0),
            r.uncacheable == s_cache(self.0),
    {
        let d = self.0;
        let flags = d & 1 | parse_flags!(!d, 7, 1) | parse_flags!(d, 10, 3) | parse_flags!(d, 51, 4)
            | parse_flags!(d, 56, 7);
        let flags = if d & USER != 0 {
            flags | parse_flags!(!d, 54, 2)
        } else {
            flags | parse_flags!(!d, 53, 2)
        };
        let priv_flags = d >> 6 & 1 | parse_flags!(!d, 11, 1) | parse_flags!(d, 55, 6);
        let cache = d & ATTR_DEVICE != 0;
        PageProperty { flags: flags as u8, uncacheable: cache, priv_flags: priv_flags as u8 }
    }

    fn pt_flags(&self) -> (r: u8)
        ensures
            r == s_pt_flags(self.0),
    {
        let bits = parse_flags!(self.0, 55, 6) | parse_flags!(self.0, 56, 7);
        bits as u8
    }

    pub fn new_page(paddr: u64, level: u8, prop: PageProperty) -> (r: PageTableEntry)
        requires
            paddr & !PHYS_ADDR_MASK == 0,  // the source's debug_assert_eq!

        ensures
            r.0 == s_new_page(paddr, level, prop.flags, prop.priv_flags, prop.uncacheable),
    {
        // FIXME (source): to avoid the Access Flag Fault, the ACCESSED bit is 1 all the time.
        let f0 = ACCESSED;
        let f1 = if level == 1 {
            f0 | NON_HUGE
        } else {
            f0
        };
        let flags = prop.flags;
        let privf = prop.priv_flags;
        let f2 = f1 | parse_flags!(flags, 0, 0) | parse_flags!(!flags, 1, 7) | parse_flags!(flags, 3, 10)
            | parse_flags!(flags, 4, 51) | parse_flags!(privf, 0, 6) | parse_flags!(!privf, 1, 11)
            | parse_flags!(privf, 6, 55) | parse_flags!(flags, 7, 56);
        let f3 = if privf & 1 != 0 {
            f2 | NO_EXECUTE_KERN | parse_flags!(!flags, 2, 54)
        } else {
            f2 | NO_EXECUTE_USER | parse_flags!(!flags, 2, 53)
        };
        let f4 = f3 | SH_INNER;
        let f5 = if prop.uncacheable {
            f4 | ATTR_DEVICE
        } else {
            f4
        };
        PageTableEntry(paddr | f5)
    }

    pub fn new_pt(paddr: u64, flags: u8) -> (r: PageTableEntry)
        requires
            paddr & !PHYS_ADDR_MASK == 0,
        ensures
            r.0 == s_new_pt(paddr, flags),
    {
        let f = VALID | NON_HUGE | parse_flags!(flags, 6, 55) | parse_flags!(flags, 7, 56);
        PageTableEntry(paddr | f)
    }

    pub fn from_repr(repr: &Repr, level: u8) -> (r: PageTableEntry)
        requires
            match *repr {
                Repr::Absent => true,
                Repr::PageTable(p, _) => p & !PHYS_ADDR_MASK == 0,
                Repr::Mapped(p, _) => p & !PHYS_ADDR_MASK == 0,
            },
        ensures
            r.0 == s_from_repr(*repr, level),
    {
        match repr {
            Repr::Absent => PageTableEntry(0),
            Repr::PageTable(paddr, flags) => Self::new_pt(*paddr, *flags),
            Repr::Mapped(paddr, prop) => Self::new_page(
                *paddr,
                level,
                PageProperty { flags: prop.flags, uncacheable: prop.uncacheable, priv_flags: prop.priv_flags },
            ),
        }
    }

    pub fn to_repr(&self, level: u8) -> (r: Repr)
        ensures
            r == s_to_repr(self.0, level),
    {
        if !self.is_present() {
            return Repr::Absent;
        }
        if self.is_last(level) {
            Repr::Mapped(self.paddr(), self.prop())
        } else {
            Repr::PageTable(self.paddr(), self.pt_flags())
        }
    }
}

// ================================================================ proofs

/// Well-formed inputs of `new_page`: only the bits the encoding uses (PageFlags R,W,X,ACCESSED,DIRTY,AVAIL2;
/// PrivilegedPageFlags USER,GLOBAL,AVAIL1), an in-range aligned-to-4K address.
pub open spec fn wf_page(paddr: u64, flags: u8, priv_flags: u8) -> bool {
    &&& paddr & !PHYS_ADDR_MASK == 0
    &&& flags & 0x60 == 0
    &&& priv_flags & 0xBC == 0
}

/// Round trip of `new_page` through Asterinas' own decoder, for every level 1..=4.
/// ACCESSED comes back set whatever the input (the source forces AF = 1).
pub proof fn lemma_new_page_roundtrip(paddr: u64, level: u8, flags: u8, priv_flags: u8, unc: bool)
    requires
        1 <= level <= 4,
        wf_page(paddr, flags, priv_flags),
    ensures
        ({
            let e = s_new_page(paddr, level, flags, priv_flags, unc);
            &&& s_is_present(e)
            &&& s_is_last(e, level)
            &&& s_paddr(e) == paddr
            &&& s_flags(e) == flags | 8
            &&& s_priv(e) == priv_flags
            &&& s_cache(e) == unc
        }),
{
    let e = s_new_page(paddr, level, flags, priv_flags, unc);
    assert(e & 0x300 == 0x300) by (bit_vector)
        requires
            e == s_new_page(paddr, level, flags, priv_flags, unc),
    ;
    assert(s_is_last(e, level)) by {
        if level != 1 {
            assert(e & 2 == 0) by (bit_vector)
                requires
                    e == s_new_page(paddr, level, flags, priv_flags, unc),
                    level != 1,
                    paddr & !PHYS_ADDR_MASK == 0,
            ;
        }
    }
    assert(e & PHYS_ADDR_MASK == paddr) by (bit_vector)
        requires
            e == s_new_page(paddr, level, flags, priv_flags, unc),
            paddr & !PHYS_ADDR_MASK == 0,
    ;
    assert(s_flags(e) == flags | 8) by (bit_vector)
        requires
            e == s_new_page(paddr, level, flags, priv_flags, unc),
            wf_page(paddr, flags, priv_flags),
    ;
    assert(s_priv(e) == priv_flags) by (bit_vector)
        requires
            e == s_new_page(paddr, level, flags, priv_flags, unc),
            wf_page(paddr, flags, priv_flags),
    ;
    assert(s_cache(e) == unc) by (bit_vector)
        requires
            e == s_new_page(paddr, level, flags, priv_flags, unc),
            wf_page(paddr, flags, priv_flags),
    ;
}

pub open spec fn wf_pt(paddr: u64, flags: u8) -> bool {
    &&& paddr & !PHYS_ADDR_MASK == 0
    &&& flags & 0x3F == 0
}

/// Round trip of `new_pt`, at every level where a table is allowed (2..=4).
pub proof fn lemma_new_pt_roundtrip(paddr: u64, level: u8, flags: u8)
    requires
        2 <= level <= 4,
        wf_pt(paddr, flags),
    ensures
        ({
            let e = s_new_pt(paddr, flags);
            &&& s_is_present(e)
            &&& !s_is_last(e, level)
            &&& s_paddr(e) == paddr
            &&& s_pt_flags(e) == flags
        }),
{
    let e = s_new_pt(paddr, flags);
    assert(e & 1 == 1 && e & 2 == 2) by (bit_vector)
        requires
            e == s_new_pt(paddr, flags),
    ;
    assert(e & PHYS_ADDR_MASK == paddr) by (bit_vector)
        requires
            e == s_new_pt(paddr, flags),
            paddr & !PHYS_ADDR_MASK == 0,
    ;
    assert(s_pt_flags(e) == flags) by (bit_vector)
        requires
            e == s_new_pt(paddr, flags),
            wf_pt(paddr, flags),
    ;
}

/// `to_repr(from_repr(r, level), level)` gives back `r` (with ACCESSED forced on) for every well-formed `r`.
pub proof fn lemma_repr_roundtrip(r: Repr, level: u8)
    requires
        1 <= level <= 4,
        match r {
            Repr::Absent => true,
            Repr::PageTable(p, f) => level >= 2 && wf_pt(p, f),
            Repr::Mapped(p, prop) => wf_page(p, prop.flags, prop.priv_flags),
        },
    ensures
        match r {
            Repr::Absent => s_to_repr(s_from_repr(r, level), level) == r,
            Repr::PageTable(p, f) => s_to_repr(s_from_repr(r, level), level) == r,
            Repr::Mapped(p, prop) => s_to_repr(s_from_repr(r, level), level) == Repr::Mapped(
                p,
                PageProperty { flags: prop.flags | 8, ..prop },
            ),
        },
{
    match r {
        Repr::Absent => {
            assert(!s_is_present(0u64)) by (bit_vector);
        },
        Repr::PageTable(p, f) => lemma_new_pt_roundtrip(p, level, f),
        Repr::Mapped(p, prop) => lemma_new_page_roundtrip(
            p,
            level,
            prop.flags,
            prop.priv_flags,
            prop.uncacheable,
        ),
    }
}

/// What the MMU sees for a page the kernel maps readable (flags.R set) at level 1..=3, given an address aligned
/// to the block size.
pub proof fn lemma_new_page_hw(paddr: u64, level: u8, flags: u8, priv_flags: u8, unc: bool)
    requires
        1 <= level <= 3,
        wf_page(paddr, flags, priv_flags),
        paddr & ((block_size(level) - 1) as u64) == 0,
        flags & 1 == 1,
    ensures
        ({
            let e = s_new_page(paddr, level, flags, priv_flags, unc);
            &&& kind(e, level) == (if level == 1 {
                Kind::Page
            } else {
                Kind::Block
            })
            &&& out_addr(e, level) == paddr
            &&& attr_indx(e) == (if unc {
                1u64
            } else {
                0u64
            })  // MAIR Attr0 normal WB / Attr1 device-nGnRnE
            &&& sh(e) == 3  // inner shareable
            &&& af(e)  // no Access Flag fault
            &&& ng(e) == (priv_flags & 2 == 0)
            &&& (ap(e) >> 1) & 1 == (if flags & 2 != 0 {
                0u64
            } else {
                1u64
            })  // AP[2] = read-only iff not writable
            &&& ap(e) & 1 == (priv_flags & 1) as u64  // AP[1] = EL0 accessible iff USER
            &&& (priv_flags & 1 != 0 ==> pxn(e))  // the kernel never executes user pages
            &&& (priv_flags & 1 == 0 ==> uxn(e))  // EL0 never executes kernel pages
            &&& (flags & 4 != 0 ==> (if priv_flags & 1 != 0 {
                !uxn(e)
            } else {
                !pxn(e)
            }))
        }),
{
    let e = s_new_page(paddr, level, flags, priv_flags, unc);
    assert(e & 1 == 1) by (bit_vector)
        requires
            e == s_new_page(paddr, level, flags, priv_flags, unc),
            flags & 1 == 1,
    ;
    if level == 1 {
        assert(e & 2 != 0) by (bit_vector)
            requires
                e == s_new_page(paddr, level, flags, priv_flags, unc),
                level == 1,
        ;
    } else {
        assert(e & 2 == 0) by (bit_vector)
            requires
                e == s_new_page(paddr, level, flags, priv_flags, unc),
                level != 1,
                paddr & !PHYS_ADDR_MASK == 0,
        ;
    }
    // out_addr: the address is block aligned, so masking the low bits changes nothing.
    assert(e & PHYS_ADDR_MASK == paddr) by (bit_vector)
        requires
            e == s_new_page(paddr, level, flags, priv_flags, unc),
            paddr & !PHYS_ADDR_MASK == 0,
    ;
    assert(out_addr(e, level) == paddr) by (bit_vector)
        requires
            e & PHYS_ADDR_MASK == paddr,
            paddr & ((block_size(level) - 1) as u64) == 0,
            1 <= level <= 3,
    ;
    assert(attr_indx(e) == (if unc {
        1u64
    } else {
        0u64
    })) by (bit_vector)
        requires
            e == s_new_page(paddr, level, flags, priv_flags, unc),
            wf_page(paddr, flags, priv_flags),
    ;
    assert(sh(e) == 3 && af(e)) by (bit_vector)
        requires
            e == s_new_page(paddr, level, flags, priv_flags, unc),
    ;
    assert(ng(e) == (priv_flags & 2 == 0)) by (bit_vector)
        requires
            e == s_new_page(paddr, level, flags, priv_flags, unc),
            wf_page(paddr, flags, priv_flags),
    ;
    assert((ap(e) >> 1) & 1 == (if flags & 2 != 0 {
        0u64
    } else {
        1u64
    }) && ap(e) & 1 == (priv_flags & 1) as u64) by (bit_vector)
        requires
            e == s_new_page(paddr, level, flags, priv_flags, unc),
            wf_page(paddr, flags, priv_flags),
    ;
    assert((priv_flags & 1 != 0 ==> pxn(e)) && (priv_flags & 1 == 0 ==> uxn(e))) by (bit_vector)
        requires
            e == s_new_page(paddr, level, flags, priv_flags, unc),
    ;
    if flags & 4 != 0 {
        if priv_flags & 1 != 0 {
            assert(uxn(e) == false) by (bit_vector)
                requires
                    e == s_new_page(paddr, level, flags, priv_flags, unc),
                    flags & 4 != 0,
                    paddr & !PHYS_ADDR_MASK == 0,
                    priv_flags & 1 != 0,
            ;
        } else {
            assert(pxn(e) == false) by (bit_vector)
                requires
                    e == s_new_page(paddr, level, flags, priv_flags, unc),
                    flags & 4 != 0,
                    paddr & !PHYS_ADDR_MASK == 0,
                    priv_flags & 1 == 0,
            ;
        }
    }
}

/// A page table entry made by `new_pt` is a hardware table descriptor that restricts nothing (table attributes
/// NSTable/APTable/UXNTable/PXNTable, bits 63:59, are zero).
pub proof fn lemma_new_pt_hw(paddr: u64, level: u8, flags: u8)
    requires
        2 <= level <= 4,
        wf_pt(paddr, flags),
    ensures
        ({
            let e = s_new_pt(paddr, flags);
            &&& kind(e, level) == Kind::Table
            &&& table_addr(e) == paddr
            &&& table_attrs(e) == 0
        }),
{
    let e = s_new_pt(paddr, flags);
    assert(e & 1 == 1 && e & 2 != 0) by (bit_vector)
        requires
            e == s_new_pt(paddr, flags),
    ;
    assert(table_addr(e) == paddr && table_attrs(e) == 0) by (bit_vector)
        requires
            e == s_new_pt(paddr, flags),
            paddr & !PHYS_ADDR_MASK == 0,
    ;
}

/// For every valid descriptor at levels 2 and 3, Asterinas' `to_repr` agrees with the MMU on block vs table.
pub proof fn lemma_decode_agrees_hw(d: u64, level: u8)
    requires
        2 <= level <= 3,
        d & 1 == 1,
    ensures
        s_is_present(d),
        s_is_last(d, level) <==> kind(d, level) == Kind::Block,
        !s_is_last(d, level) <==> kind(d, level) == Kind::Table,
{
}

/// A "PROT_NONE" entry: no VALID bit but SH_INNER set. Asterinas treats it as present (Mapped, R clear);
/// the MMU faults on it. This is intended (new_page documents it), and it is the only present-but-invalid shape
/// among kernel-made entries.
pub proof fn lemma_prot_none(d: u64, level: u8)
    requires
        d & 1 == 0,
        d & SH_INNER != 0,
    ensures
        s_is_present(d),
        kind(d, level) == Kind::Invalid,
        s_to_repr(d, level) is Mapped || s_to_repr(d, level) is PageTable,
{
}

/// FINDING (architecture mismatch): descriptors with bits[1:0] == 0b01 are reserved at ARM level 3 (Asterinas
/// level 1) and ARM level 0 (Asterinas level 4), so the MMU raises a translation fault, but Asterinas' `is_last`
/// reads them as `Mapped`. Kernel-made entries avoid the first case (new_page sets NON_HUGE at level 1). The
/// second case is what `new_page(.., level = 4, ..)` would produce: it would be a mapping the MMU never honours.
pub proof fn lemma_reserved_encodings(d: u64, level: u8)
    requires
        d & 3 == 1,
        level == 1 || level == 4,
    ensures
        kind(d, level) == Kind::Invalid,
        s_is_last(d, level),
        s_is_present(d),
{
    assert(d & 1 != 0 && d & 2 == 0) by (bit_vector)
        requires
            d & 3 == 1,
    ;
}

pub proof fn lemma_level4_mapping_is_hw_invalid(paddr: u64, flags: u8, priv_flags: u8, unc: bool)
    requires
        wf_page(paddr, flags, priv_flags),
        flags & 1 == 1,
    ensures
        kind(s_new_page(paddr, 4, flags, priv_flags, unc), 4) == Kind::Invalid,
{
    let e = s_new_page(paddr, 4, flags, priv_flags, unc);
    assert(e & 3 == 1) by (bit_vector)
        requires
            e == s_new_page(paddr, 4, flags, priv_flags, unc),
            flags & 1 == 1,
            paddr & !PHYS_ADDR_MASK == 0,
    ;
    assert(e & 1 != 0 && e & 2 == 0) by (bit_vector)
        requires
            e & 3 == 1,
    ;
}

/// A hardware block/page descriptor's output address is aligned to its block size, whatever the descriptor bits.
pub proof fn lemma_out_addr_aligned(d: u64, level: u8)
    requires
        1 <= level <= 4,
    ensures
        out_addr(d, level) & ((block_size(level) - 1) as u64) == 0,
        out_addr(d, level) & 0xFFF == 0,
        out_addr(d, level) < 0x1_0000_0000_0000,
{
    if level == 1 {
        assert(out_addr(d, 1) & 0xFFF == 0 && out_addr(d, 1) < 0x1_0000_0000_0000 && out_addr(d, 1) & 0xFFF == 0) by (bit_vector);
    } else if level == 2 {
        assert(out_addr(d, 2) & 0x1F_FFFF == 0 && out_addr(d, 2) < 0x1_0000_0000_0000 && out_addr(d, 2) & 0xFFF == 0) by (bit_vector);
    } else if level == 3 {
        assert(out_addr(d, 3) & 0x3FFF_FFFF == 0 && out_addr(d, 3) < 0x1_0000_0000_0000 && out_addr(d, 3) & 0xFFF == 0) by (bit_vector);
    } else {
        assert(out_addr(d, 4) & 0x7F_FFFF_FFFF == 0 && out_addr(d, 4) < 0x1_0000_0000_0000 && out_addr(d, 4) & 0xFFF == 0) by (bit_vector);
    }
}

// ---------------------------------------------------------------- boot-time translation control (bsp_boot.S)
pub open spec fn tcr_el1() -> u64 {
    // TCR_TG_4K | TCR_SH_INNER | TCR_ORGN_WBWA | TCR_IRGN_WBWA | TCR_TSZ_48 | TCR_IPS_48
    ((2u64 << 30) | (0u64 << 14)) | ((3u64 << 28) | (3u64 << 12)) | ((1u64 << 26) | (1u64 << 10))
        | ((1u64 << 24) | (1u64 << 8)) | ((16u64 << 16) | (16u64 << 0)) | (5u64 << 32)
}

/// The TCR_EL1 value written by the boot assembly selects exactly the modelled configuration.
pub proof fn lemma_tcr_config()
    ensures
        tcr_el1() & 0x3F == 16,  // T0SZ = 16 -> 64 - 16 = 48 VA bits
        (tcr_el1() >> 16) & 0x3F == 16,  // T1SZ = 16
        (tcr_el1() >> 14) & 3 == 0,  // TG0 = 4 KiB
        (tcr_el1() >> 30) & 3 == 2,  // TG1 = 4 KiB
        (tcr_el1() >> 32) & 7 == 5,  // IPS = 48-bit PA
        (tcr_el1() >> 39) & 3 == 0,  // HA = HD = 0: no hardware AF/dirty update, hence new_page forces AF = 1
        (tcr_el1() >> 7) & 1 == 0,  // EPD0 = 0
        (tcr_el1() >> 23) & 1 == 0,  // EPD1 = 0
{
    assert(tcr_el1() & 0x3F == 16 && (tcr_el1() >> 16) & 0x3F == 16 && (tcr_el1() >> 14) & 3 == 0
        && (tcr_el1() >> 30) & 3 == 2 && (tcr_el1() >> 32) & 7 == 5 && (tcr_el1() >> 39) & 3 == 0
        && (tcr_el1() >> 7) & 1 == 0 && (tcr_el1() >> 23) & 1 == 0) by (bit_vector);
}

/// MAIR_EL1 = MAIR_0WBWA | MAIR_1DEVICE: Attr0 = 0xFF (Normal, WB, R/W-allocate), Attr1 = 0x00 (Device-nGnRnE).
/// AttrIndx 0 / 1 are the only values `new_page` produces (see lemma_new_page_hw).
pub open spec fn mair_el1() -> u64 {
    (0xFFu64 << 0) | (0x00u64 << 8)
}

pub proof fn lemma_mair_config()
    ensures
        mair_el1() & 0xFF == 0xFF,
        (mair_el1() >> 8) & 0xFF == 0,
{
    assert(mair_el1() & 0xFF == 0xFF && (mair_el1() >> 8) & 0xFF == 0) by (bit_vector);
}

fn main() {}

} // verus!
