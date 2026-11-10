//! The modem firmware image, Linux 7.1 `drivers/soc/qcom/mdt_loader.c` (GPL-2.0-only). Adapted from narf
//! `drivers/soc/qcom/src/mdt.rs` (GPL-2.0-or-later, not run on silicon), without split images (`.bNN`): Samsung's
//! `qcmpss7180_nm.mbn` is a single file. The section-header check of `mdt_header_valid` was missing there and is added.
//!
//! Program header 0 covers the ELF header and program headers. The one whose `p_flags & 7 << 24` is 2 << 24 holds the
//! hash and signature. `metadata` is header 0's bytes then the hash segment's, given to the secure world by
//! `PAS_INIT_IMAGE`. Loadable segments go in the carve-out at `p_paddr` minus the relocation base.

use alloc::vec::Vec;

pub const PT_LOAD: u32 = 1;
pub const QCOM_MDT_TYPE_MASK: u32 = 7 << 24;
pub const QCOM_MDT_TYPE_HASH: u32 = 2 << 24;
pub const QCOM_MDT_RELOCATABLE: u32 = 1 << 27;
const EHDR_SIZE: usize = 52;
const PHDR_SIZE: usize = 32;
const SHDR_SIZE: usize = 40;
const SZ_4K: u64 = 0x1000;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum MdtError {
    BadHeader,
    NoHashSegment,
    NoSegments,
    SegmentOutOfRange(usize),
    FileszExceedsMemsz(usize),
    Truncated(usize),
}

/// `struct elf32_phdr`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Phdr {
    pub p_type: u32,
    pub p_offset: u32,
    pub p_vaddr: u32,
    pub p_paddr: u32,
    pub p_filesz: u32,
    pub p_memsz: u32,
    pub p_flags: u32,
    pub p_align: u32,
}

impl Phdr {
    /// `mdt_phdr_loadable`.
    pub fn loadable(&self) -> bool {
        self.p_type == PT_LOAD
            && (self.p_flags & QCOM_MDT_TYPE_MASK) != QCOM_MDT_TYPE_HASH
            && self.p_memsz != 0
    }
    pub fn relocatable(&self) -> bool {
        self.p_flags & QCOM_MDT_RELOCATABLE != 0
    }
}

/// What the secure world is told about the image's place in memory.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Span {
    pub min: u64,
    /// 4 KiB aligned end, computed as `__qcom_mdt_pas_init` does.
    pub max: u64,
    pub relocatable: bool,
}

#[derive(Clone, Copy)]
pub struct Mdt<'a> {
    data: &'a [u8],
    entry: u32,
    phoff: usize,
    phnum: usize,
}

fn le32(b: &[u8], o: usize) -> Option<u32> {
    let s = b.get(o..o.checked_add(4)?)?;
    Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn le16(b: &[u8], o: usize) -> Option<u16> {
    let s = b.get(o..o.checked_add(2)?)?;
    Some(u16::from_le_bytes([s[0], s[1]]))
}

impl<'a> Mdt<'a> {
    /// `mdt_header_valid`.
    pub fn parse(data: &'a [u8]) -> Result<Self, MdtError> {
        if data.len() < EHDR_SIZE || &data[..4] != b"\x7fELF" {
            return Err(MdtError::BadHeader);
        }
        let get32 = |o| le32(data, o).ok_or(MdtError::BadHeader);
        let get16 = |o| le16(data, o).ok_or(MdtError::BadHeader);
        let entry = get32(24)?;
        let phoff = get32(28)? as usize;
        let shoff = get32(32)? as usize;
        let phentsize = get16(42)? as usize;
        let phnum = get16(44)? as usize;
        let shentsize = get16(46)? as usize;
        let shnum = get16(48)? as usize;
        if phentsize != PHDR_SIZE
            || phoff
                .checked_add(phnum * PHDR_SIZE)
                .is_none_or(|end| end > data.len())
        {
            return Err(MdtError::BadHeader);
        }
        if (shentsize != 0 || shnum != 0)
            && (shentsize != SHDR_SIZE
                || shoff
                    .checked_add(shnum * SHDR_SIZE)
                    .is_none_or(|end| end > data.len()))
        {
            return Err(MdtError::BadHeader);
        }
        Ok(Self {
            data,
            entry,
            phoff,
            phnum,
        })
    }

    pub fn entry(&self) -> u32 {
        self.entry
    }

    pub fn phnum(&self) -> usize {
        self.phnum
    }

    pub fn phdr(&self, i: usize) -> Option<Phdr> {
        if i >= self.phnum {
            return None;
        }
        let o = self.phoff + i * PHDR_SIZE;
        let d = self.data;
        Some(Phdr {
            p_type: le32(d, o)?,
            p_offset: le32(d, o + 4)?,
            p_vaddr: le32(d, o + 8)?,
            p_paddr: le32(d, o + 12)?,
            p_filesz: le32(d, o + 16)?,
            p_memsz: le32(d, o + 20)?,
            p_flags: le32(d, o + 24)?,
            p_align: le32(d, o + 28)?,
        })
    }

    fn phdrs(&self) -> impl Iterator<Item = (usize, Phdr)> + '_ {
        (0..self.phnum).filter_map(move |i| self.phdr(i).map(|p| (i, p)))
    }

    /// The span loop of `__qcom_mdt_pas_init`. Its `max` keeps the alignment from the segment that last raised it.
    pub fn span(&self) -> Result<Span, MdtError> {
        let (mut min, mut max, mut relocatable) = (u64::MAX, 0u64, false);
        for (_, p) in self.phdrs().filter(|(_, p)| p.loadable()) {
            relocatable |= p.relocatable();
            min = min.min(p.p_paddr as u64);
            let end = p.p_paddr as u64 + p.p_memsz as u64;
            if end > max {
                max = (end + SZ_4K - 1) & !(SZ_4K - 1);
            }
        }
        if min >= max {
            return Err(MdtError::NoSegments);
        }
        Ok(Span {
            min,
            max,
            relocatable,
        })
    }

    /// `qcom_mdt_bins_are_split`: this module refuses to load such an image.
    pub fn is_split(&self) -> bool {
        let n = self.data.len() as u64;
        self.phdrs().any(|(_, p)| {
            p.p_filesz != 0 && (p.p_offset as u64 > n || p.p_offset as u64 + p.p_filesz as u64 > n)
        })
    }

    /// `qcom_mdt_read_metadata`, single-file images.
    pub fn metadata(&self) -> Result<Vec<u8>, MdtError> {
        if self.phnum < 2 {
            return Err(MdtError::NoHashSegment);
        }
        let p0 = self.phdr(0).ok_or(MdtError::BadHeader)?;
        if p0.p_type == PT_LOAD {
            return Err(MdtError::NoHashSegment);
        }
        let (_, hash) = self
            .phdrs()
            .skip(1)
            .find(|(_, p)| p.p_flags & QCOM_MDT_TYPE_MASK == QCOM_MDT_TYPE_HASH)
            .ok_or(MdtError::NoHashSegment)?;
        let (ehdr_size, hash_size) = (p0.p_filesz as usize, hash.p_filesz as usize);
        let head = self.data.get(..ehdr_size).ok_or(MdtError::Truncated(0))?;
        let o = hash.p_offset as usize;
        let tail = o
            .checked_add(hash_size)
            .and_then(|end| self.data.get(o..end))
            .ok_or(MdtError::Truncated(0))?;
        let mut out = Vec::with_capacity(ehdr_size + hash_size);
        out.extend_from_slice(head);
        out.extend_from_slice(tail);
        Ok(out)
    }

    /// `qcom_mdt_load_no_init` for a carve-out of `mem_size` at `mem_phys`. Calls `copy(offset, bytes)` per segment
    /// and `zero(offset, len)` for the rest of its memory size. Everything is checked before the first call, so a
    /// bad image never half loads. Returns the relocation base.
    pub fn load(
        &self,
        mem_phys: u64,
        mem_size: u64,
        mut copy: impl FnMut(u64, &[u8]),
        mut zero: impl FnMut(u64, u64),
    ) -> Result<u64, MdtError> {
        let span = self.span()?;
        let reloc = if span.relocatable { span.min } else { mem_phys };
        for (i, p) in self.phdrs().filter(|(_, p)| p.loadable()) {
            let offset = (p.p_paddr as u64).checked_sub(reloc);
            if offset.is_none_or(|o| o + p.p_memsz as u64 > mem_size) {
                return Err(MdtError::SegmentOutOfRange(i));
            }
            if p.p_filesz > p.p_memsz {
                return Err(MdtError::FileszExceedsMemsz(i));
            }
            let end = p.p_offset as u64 + p.p_filesz as u64;
            if p.p_filesz != 0 && end > self.data.len() as u64 {
                return Err(MdtError::Truncated(i));
            }
        }
        for (_, p) in self.phdrs().filter(|(_, p)| p.loadable()) {
            let offset = p.p_paddr as u64 - reloc;
            let (so, filesz) = (p.p_offset as usize, p.p_filesz as usize);
            if filesz != 0 {
                copy(offset, &self.data[so..so + filesz]);
            }
            if p.p_memsz > p.p_filesz {
                zero(offset + p.p_filesz as u64, (p.p_memsz - p.p_filesz) as u64);
            }
        }
        Ok(reloc)
    }
}
