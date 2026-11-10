//! Copy engine rings of the WCN3990, as Linux `ce.c` drives them over SNOC.
//!
//! Derived from Linux `drivers/net/wireless/ath/ath10k/ce.c`, `ce.h` and `hw.c` (the `wcn3990_*` register blocks)
//! (SPDX ISC; Copyright (c) 2005-2011 Atheros Communications Inc., 2011-2017 Qualcomm Atheros, Inc., 2018 The Linux
//! Foundation, 2022 Qualcomm Innovation Center, Inc., Qualcomm Technologies, Inc. and/or its subsidiaries).
//!
//! Only the WCN3990 variant exists here: 64-bit descriptors (`ce_64_ops`), shadow registers for source write
//! indices (`shadow_reg_support`) and read indices kept in host memory (`rri_on_ddr`). `tests/ce_trace.rs` runs the
//! unchanged `ce.c` against a fake register block (oracle/ce_oracle.c) and replays the same scenario here; the two
//! register/descriptor traces must be identical.
//!
//! * [`Regs`] is the `membase` window (8 MiB at 0x18800000), by byte offset. [`DescMem`] is the memory the target
//!   shares: descriptor rings and the RRI array, by byte offset into one arena, with an explicit [`Barrier`] hook.
//! * [`hw`] holds the register offsets and masks (`wcn3990_regs`, `wcn3990_ce_regs`, `ath10k_hw_values`).
//! * [`Desc64`] is `struct ce_desc_64`; [`Ring`] is `struct ath10k_ce_ring`; [`Pipe`] is `struct ath10k_ce_pipe`.
//! * [`CopyEngine`] owns the twelve pipes and the RRI array. Its methods are the `_nolock` functions of `ce.c`
//!   (callers serialise): [`CopyEngine::alloc_rri`], [`CopyEngine::alloc_pipe`] (bookkeeping only, the caller
//!   reserves the DMA memory), [`CopyEngine::init_pipe`] / [`CopyEngine::deinit_pipe`] (the register programming of
//!   `ath10k_ce_init_src_ring`/`_dest_ring`), [`CopyEngine::send`] (`_ath10k_ce_send_nolock_64`),
//!   [`CopyEngine::rx_post_buf`] (`__ath10k_ce_rx_post_buf_64`), [`CopyEngine::completed_recv_next`],
//!   [`CopyEngine::completed_send_next`], [`CopyEngine::rx_update_write_idx`], and the interrupt side:
//!   [`CopyEngine::per_engine_service`], [`CopyEngine::service_any`], [`CopyEngine::enable_interrupt`] and friends.
//!
//! Places where this deliberately differs from the C, because the C trusts memory the firmware writes:
//! the source read index from RRI memory is masked to the ring size on the completion path (the C compares the raw
//! 16-bit value, so a bad value never equals `sw_index` and the caller's completion loop never ends); all indices
//! are masked before they index anything; a send longer than the 16-bit length field is refused instead of
//! truncated; a missing shadow register address is an error instead of a write to offset 0.

use alloc::vec::Vec;

use crate::snoc_config::{CE_COUNT, CeAttr, attr};

/// Register access to the `membase` window, by byte offset.
pub trait Regs {
    fn read32(&mut self, offset: usize) -> u32;
    fn write32(&mut self, offset: usize, value: u32);
}

/// Why a barrier is being asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Barrier {
    /// Descriptor stores must be visible to the device before the next register write tells it to look
    /// (`dma_wmb()` before the doorbell).
    Release,
    /// An index or descriptor the device wrote must not be read ahead of the register or interrupt that said it was
    /// ready (`dma_rmb()`).
    Acquire,
}

/// Memory shared with the copy engine: descriptor rings and the RRI array, addressed by byte offset into one
/// arena the integration chooses (typically the offset from a DMA-coherent allocation's base).
///
/// Everything is little endian. The 64-bit accessors default to two 32-bit ones, low word first, which is what
/// the descriptor layout needs when the bus cannot do 64-bit accesses.
pub trait DescMem {
    fn read32(&mut self, offset: usize) -> u32;
    fn write32(&mut self, offset: usize, value: u32);
    fn read64(&mut self, offset: usize) -> u64 {
        let lo = self.read32(offset);
        let hi = self.read32(offset + 4);
        u64::from(lo) | (u64::from(hi) << 32)
    }
    fn write64(&mut self, offset: usize, value: u64) {
        self.write32(offset, value as u32);
        self.write32(offset + 4, (value >> 32) as u32);
    }
    /// A 16-bit store inside an aligned word (the descriptor's `nbytes` and `flags`). Read-modify-write unless the
    /// implementation can do better.
    fn write16(&mut self, offset: usize, value: u16) {
        let aligned = offset & !3;
        let shift = (offset & 2) * 8;
        let old = self.read32(aligned);
        self.write32(
            aligned,
            (old & !(0xffff << shift)) | (u32::from(value) << shift),
        );
    }
    fn read16(&mut self, offset: usize) -> u16 {
        let w = self.read32(offset & !3);
        (w >> ((offset & 2) * 8)) as u16
    }
    fn barrier(&mut self, _kind: Barrier) {}
}

/// WCN3990 register map (`hw.c`: `wcn3990_regs`, `wcn3990_ce_regs`, `wcn3990_values`).
pub mod hw {
    /// `ce_wrapper_base_address`.
    pub const CE_WRAPPER_BASE: usize = 0x0024_C000;
    /// `ce0_base_address`.
    pub const CE0_BASE: usize = 0x0024_0000;
    /// `ce1_base_address - ce0_base_address`.
    pub const CE_STRIDE: usize = 0x1000;
    /// `CE_WRAPPER_INTERRUPT_SUMMARY_ADDRESS`, relative to the wrapper.
    pub const CE_WRAPPER_INTERRUPT_SUMMARY: usize = 0x0000;
    /// `ce_wrap_intr_sum_host_msi_lsb` / `_mask`.
    pub const INTR_SUM_HOST_MSI_LSB: u32 = 12;
    pub const INTR_SUM_HOST_MSI_MASK: u32 = 0x00ff_f000;

    pub const SR_BASE_ADDR_LO: usize = 0x00;
    pub const SR_BASE_ADDR_HI: usize = 0x04;
    pub const SR_SIZE: usize = 0x08;
    pub const DR_BASE_ADDR_LO: usize = 0x0c;
    pub const DR_BASE_ADDR_HI: usize = 0x10;
    pub const DR_SIZE: usize = 0x14;
    pub const CTRL1: usize = 0x18;
    pub const HOST_IE: usize = 0x2c;
    /// `wm_regs->addr`: interrupt status (write 1s to clear).
    pub const HOST_IS: usize = 0x30;
    pub const MISC_IE: usize = 0x34;
    pub const SR_WR_INDEX: usize = 0x3c;
    pub const DST_WR_INDEX: usize = 0x40;
    pub const CURRENT_SRRI: usize = 0x44;
    pub const CURRENT_DRRI: usize = 0x48;
    pub const SRC_WM: usize = 0x4c;
    pub const DST_WM: usize = 0x50;
    /// `ce_rri_low` / `ce_rri_high` (absolute, in the wrapper).
    pub const CE_RRI_LOW: usize = 0x0024_C004;
    pub const CE_RRI_HIGH: usize = 0x0024_C008;

    /// `ctrl1_regs`: byte swap and max length fields of CTRL1.
    pub const CTRL1_SRC_RING_LSB: u32 = 0x10;
    pub const CTRL1_SRC_RING_MASK: u32 = 1 << 17;
    pub const CTRL1_DST_RING_LSB: u32 = 0x12;
    pub const CTRL1_DST_RING_MASK: u32 = 1 << 18;
    pub const CTRL1_DMAX_LSB: u32 = 0;
    pub const CTRL1_DMAX_MASK: u32 = 0xffff;
    /// `upd->mask`: set in CTRL1 of every engine by `ath10k_ce_alloc_rri` so the target mirrors read indices.
    pub const CTRL1_UPD_MASK: u32 = 0x0008_0000;

    /// `host_ie->copy_complete->mask`.
    pub const HOST_IE_COPY_COMPLETE: u32 = 0x1;
    /// `wm_regs`: `cc_mask`, `wm_mask` and the four individual watermark bits.
    pub const WM_CC_MASK: u32 = 0x01;
    pub const WM_MASK: u32 = 0x1e;
    pub const WM_DSTR_LMASK: u32 = 0x10;
    pub const WM_DSTR_HMASK: u32 = 0x08;
    pub const WM_SRCR_LMASK: u32 = 0x04;
    pub const WM_SRCR_HMASK: u32 = 0x02;
    /// `misc_regs->err_mask`.
    pub const MISC_ERR_MASK: u32 = 0x3e0;

    /// `wm_srcr` / `wm_dstr`: low watermark in bits 31:16, high in 15:0 (lsb 0x10 and 0).
    pub const WM_LOW_LSB: u32 = 0x10;
    pub const WM_LOW_MASK: u32 = 0xffff_0000;
    pub const WM_HIGH_LSB: u32 = 0;
    pub const WM_HIGH_MASK: u32 = 0x0000_ffff;

    /// `ce_desc_meta_data_mask` / `_lsb`: the transfer id sits in descriptor flags bits 15:4.
    pub const DESC_META_DATA_MASK: u32 = 0xfff0;
    pub const DESC_META_DATA_LSB: u32 = 4;

    /// `shadow_sr_wr_ind_addr`: the shadow source write index registers, by engine.
    pub const fn shadow_sr_wr_index(ce_id: usize) -> Option<usize> {
        match ce_id {
            0 => Some(0x0003_2000),
            3 => Some(0x0003_200C),
            4 => Some(0x0003_2010),
            5 => Some(0x0003_2014),
            7 => Some(0x0003_201C),
            _ => None,
        }
    }

    /// `ath10k_ce_base_address`.
    pub const fn ce_base(ce_id: usize) -> usize {
        CE0_BASE + CE_STRIDE * ce_id
    }
}

/// `CE_DESC_FLAGS_GATHER` (the 16-bit `flags` field of the descriptor).
pub const DESC_FLAGS_GATHER: u16 = 1 << 0;
/// `CE_DESC_FLAGS_BYTE_SWAP`.
pub const DESC_FLAGS_BYTE_SWAP: u16 = 1 << 1;
/// `CE_WCN3990_DESC_FLAGS_GATHER`: the gather bit of the 64-bit descriptor's second address word.
pub const WCN3990_DESC_FLAGS_GATHER: u32 = 1 << 31;
/// `CE_DESC_ADDR_MASK`: descriptors carry 35 address bits.
pub const DESC_ADDR_MASK: u64 = (1 << 35) - 1;
/// `CE_DESC_ADDR_HI_MASK`.
pub const DESC_ADDR_HI_MASK: u32 = 0x1f;
/// `CE_DDR_RRI_MASK` and `CE_DDR_DRRI_SHIFT`.
pub const DDR_RRI_MASK: u32 = 0xffff;
pub const DDR_DRRI_SHIFT: u32 = 16;
/// `CE_DESC_RING_ALIGN`.
pub const DESC_RING_ALIGN: usize = 8;
/// `sizeof(struct ce_desc_64)`.
pub const DESC_SIZE: usize = 16;
/// `CE_SEND_FLAG_GATHER` and `CE_SEND_FLAG_BYTE_SWAP` (arguments of [`CopyEngine::send`]).
pub const SEND_FLAG_GATHER: u32 = 0x0001_0000;
pub const SEND_FLAG_BYTE_SWAP: u32 = 1;

/// `struct ce_desc_64`, 16 bytes in memory:
/// `addr: __le64` (low word at +0, high word at +4), `nbytes: __le16` at +8, `flags: __le16` at +10,
/// `toeplitz_hash_result: __le32` at +12.
///
/// For a source descriptor the high address word is not a clean address: `_ath10k_ce_send_nolock_64` stores
/// `upper_32_bits(buffer) & 0x1f` ORed with the caller's send flags there, plus bit 31 for gather. The two words
/// are therefore kept apart here.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Desc64 {
    pub addr_lo: u32,
    pub addr_hi: u32,
    pub nbytes: u16,
    pub flags: u16,
    pub toeplitz_hash_result: u32,
}

impl Desc64 {
    pub fn to_le_bytes(&self) -> [u8; DESC_SIZE] {
        let mut out = [0u8; DESC_SIZE];
        out[0..4].copy_from_slice(&self.addr_lo.to_le_bytes());
        out[4..8].copy_from_slice(&self.addr_hi.to_le_bytes());
        out[8..10].copy_from_slice(&self.nbytes.to_le_bytes());
        out[10..12].copy_from_slice(&self.flags.to_le_bytes());
        out[12..16].copy_from_slice(&self.toeplitz_hash_result.to_le_bytes());
        out
    }

    pub fn from_le_bytes(b: &[u8; DESC_SIZE]) -> Self {
        let w = |i: usize| u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
        Desc64 {
            addr_lo: w(0),
            addr_hi: w(4),
            nbytes: u16::from_le_bytes([b[8], b[9]]),
            flags: u16::from_le_bytes([b[10], b[11]]),
            toeplitz_hash_result: w(12),
        }
    }

    /// The transfer id of a source descriptor (`MS(flags, CE_DESC_FLAGS_META_DATA)`).
    pub fn transfer_id(&self) -> u32 {
        (u32::from(self.flags) & hw::DESC_META_DATA_MASK) >> hw::DESC_META_DATA_LSB
    }

    /// The 35-bit buffer address, as a destination descriptor holds it.
    pub fn buffer_addr(&self) -> u64 {
        (u64::from(self.addr_hi) << 32 | u64::from(self.addr_lo)) & DESC_ADDR_MASK
    }
}

/// Errors, with the errno the C returns in brackets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CeError {
    /// No such engine, or it has no ring of that kind, or the ring sizes are unusable (`-EIO`/`-EINVAL`).
    BadPipe,
    /// Source ring full (`-ENOSR`).
    NoSpace,
    /// Destination ring full (`-ENOSPC`).
    NoBuffers,
    /// Nothing completed yet (`-EIO`).
    NotReady,
    /// The device reads back all ones (`-ENODEV`).
    NoDevice,
    /// Crash flush in progress (`-ESHUTDOWN`).
    Shutdown,
    /// RRI memory is needed and [`CopyEngine::alloc_rri`] has not been called.
    NoRri,
    /// The send does not fit the 16-bit length field.
    TooLong,
    /// A source ring was asked to ring a shadow register this engine does not have.
    NoShadowReg,
}

/// Where a ring lives: its offset in the [`DescMem`] arena and the address the device uses for it. Both are
/// aligned up to [`DESC_RING_ALIGN`] by [`Ring::new`], as `PTR_ALIGN`/`ALIGN` do in `ath10k_ce_alloc_src_ring_64`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RingPlacement {
    pub host_offset: usize,
    pub bus_addr: u64,
}

/// Bytes to reserve for a ring of `nentries` (before rounding up to a power of two it would not fit; pass the
/// rounded value from [`ring_entries`]): descriptors plus alignment slack.
pub const fn ring_mem_size(nentries: usize) -> usize {
    nentries * DESC_SIZE + DESC_RING_ALIGN
}

/// `roundup_pow_of_two(attr.nentries)`, refused when it exceeds what the 16-bit indices can express.
pub fn ring_entries(requested: u32) -> Option<u32> {
    if requested == 0 || requested > 0x1_0000 {
        return None;
    }
    requested.checked_next_power_of_two()
}

fn align_up(v: u64, a: u64) -> u64 {
    (v + a - 1) & !(a - 1)
}

/// `CE_RING_DELTA`: free-running distance modulo the ring size, in the C's int arithmetic.
pub const fn ring_delta(mask: u32, from: u32, to: u32) -> u32 {
    to.wrapping_sub(from) & mask
}

/// `struct ath10k_ce_ring`.
#[derive(Debug)]
pub struct Ring {
    nentries: u32,
    mask: u32,
    /// Next index software will process (destination), or last send whose completion software processed (source).
    pub sw_index: u32,
    /// Cached copy of the index software wrote to the device.
    pub write_index: u32,
    /// Cached copy of the device's read index (source rings only).
    pub hw_index: u32,
    host_base: usize,
    bus_base: u64,
    contexts: Vec<Option<u64>>,
}

impl Ring {
    /// `nentries` is the already rounded power of two.
    pub fn new(nentries: u32, at: RingPlacement) -> Result<Ring, CeError> {
        if !nentries.is_power_of_two() || nentries > 0x1_0000 {
            return Err(CeError::BadPipe);
        }
        Ok(Ring {
            nentries,
            mask: nentries - 1,
            sw_index: 0,
            write_index: 0,
            hw_index: 0,
            host_base: align_up(at.host_offset as u64, DESC_RING_ALIGN as u64) as usize,
            bus_base: align_up(at.bus_addr, DESC_RING_ALIGN as u64),
            contexts: alloc::vec![None; nentries as usize],
        })
    }

    pub fn nentries(&self) -> u32 {
        self.nentries
    }
    pub fn mask(&self) -> u32 {
        self.mask
    }
    /// `base_addr_ce_space`: what the device is programmed with.
    pub fn bus_base(&self) -> u64 {
        self.bus_base
    }
    /// `base_addr_owner_space`, as an arena offset.
    pub fn host_base(&self) -> usize {
        self.host_base
    }
    fn desc_off(&self, idx: u32) -> usize {
        self.host_base + (idx & self.mask) as usize * DESC_SIZE
    }
    /// Offset of descriptor `idx` (masked) in the arena, for tests and for a target model.
    pub fn desc_offset(&self, idx: u32) -> usize {
        self.desc_off(idx)
    }
    /// Free entries in the ring as software sees it: `CE_RING_DELTA(mask, write_index, sw_index - 1)`.
    pub fn free_entries(&self) -> u32 {
        ring_delta(self.mask, self.write_index, self.sw_index.wrapping_sub(1))
    }
}

/// `struct ath10k_ce_pipe` (one copy engine's state).
#[derive(Debug)]
pub struct Pipe {
    pub id: u8,
    pub ctrl_addr: usize,
    pub attr_flags: u32,
    pub src_sz_max: u32,
    pub src_ring: Option<Ring>,
    pub dest_ring: Option<Ring>,
    /// `send_cb` is set (only if the engine has a source ring).
    pub has_send_cb: bool,
    /// `recv_cb` is set (only if the engine has a destination ring).
    pub has_recv_cb: bool,
}

/// What `ath10k_ce_per_engine_service` asks the caller to run after it cleared the interrupt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Service {
    pub run_recv_cb: bool,
    pub run_send_cb: bool,
}

/// All twelve copy engines of one WCN3990.
#[derive(Debug)]
pub struct CopyEngine {
    poll_from_memory: bool,
    pipes: Vec<Option<Pipe>>,
    /// Arena offset of the RRI array (`u32[CE_COUNT]`), once [`CopyEngine::alloc_rri`] ran.
    rri_offset: Option<usize>,
    /// `ATH10K_FLAG_CRASH_FLUSH`.
    pub crash_flush: bool,
}

impl Default for CopyEngine {
    fn default() -> Self {
        Self::new()
    }
}

fn rmw<R: Regs>(regs: &mut R, off: usize, f: impl FnOnce(u32) -> u32) {
    let v = regs.read32(off);
    regs.write32(off, f(v));
}

/// `ath10k_set_ring_byte`.
const fn set_field(v: u32, lsb: u32, mask: u32) -> u32 {
    v.wrapping_shl(lsb) & mask
}

impl CopyEngine {
    pub fn new() -> Self {
        let mut pipes = Vec::with_capacity(CE_COUNT);
        for _ in 0..CE_COUNT {
            pipes.push(None);
        }
        CopyEngine {
            pipes,
            poll_from_memory: false,
            rri_offset: None,
            crash_flush: false,
        }
    }

    pub fn pipe(&self, id: usize) -> Option<&Pipe> {
        self.pipes.get(id).and_then(|p| p.as_ref())
    }

    /// The source ring of engine `id`, for a target model or a test that needs to poke at the cached indices.
    pub fn src_ring_mut(&mut self, id: usize) -> Option<&mut Ring> {
        self.pipes
            .get_mut(id)
            .and_then(|p| p.as_mut())
            .and_then(|p| p.src_ring.as_mut())
    }

    /// The destination ring of engine `id`.
    pub fn dest_ring_mut(&mut self, id: usize) -> Option<&mut Ring> {
        self.pipes
            .get_mut(id)
            .and_then(|p| p.as_mut())
            .and_then(|p| p.dest_ring.as_mut())
    }

    fn pipe_mut(&mut self, id: usize) -> Result<&mut Pipe, CeError> {
        self.pipes
            .get_mut(id)
            .and_then(|p| p.as_mut())
            .ok_or(CeError::BadPipe)
    }

    // ---- RRI memory (read indices the target mirrors into host memory) -----------------------------------------

    /// For an owner that polls instead of taking interrupts: every engine's send index comes from the array the device
    /// mirrors, the source `_ath10k_ce_send_nolock_64` already trusts, not from a register of a block that may be asleep.
    pub fn poll_from_memory(&mut self) {
        self.poll_from_memory = true;
    }

    /// `ath10k_ce_alloc_rri`, minus the allocation: tell the device where the array is and ask every engine to
    /// update it. `host_offset`/`bus_addr` locate `CE_COUNT * 4` bytes of zeroed shared memory.
    pub fn alloc_rri<R: Regs>(&mut self, regs: &mut R, host_offset: usize, bus_addr: u64) {
        self.rri_offset = Some(host_offset);
        regs.write32(hw::CE_RRI_LOW, bus_addr as u32);
        regs.write32(
            hw::CE_RRI_HIGH,
            ((bus_addr >> 32) as u32) & DESC_ADDR_HI_MASK,
        );
        for i in 0..CE_COUNT {
            let base = hw::ce_base(i);
            let mut v = regs.read32(base + hw::CTRL1);
            v |= hw::CTRL1_UPD_MASK;
            regs.write32(base + hw::CTRL1, v);
        }
    }

    /// The word the device mirrors for this engine, for a log.
    pub fn rri_raw<M: DescMem>(&self, mem: &mut M, ce_id: usize) -> Option<u32> {
        self.rri_word(mem, ce_id).ok()
    }

    fn rri_word<M: DescMem>(&self, mem: &mut M, ce_id: usize) -> Result<u32, CeError> {
        if ce_id >= CE_COUNT {
            return Err(CeError::BadPipe);
        }
        let off = self.rri_offset.ok_or(CeError::NoRri)?;
        mem.barrier(Barrier::Acquire);
        Ok(mem.read32(off + 4 * ce_id))
    }

    /// `ath10k_ce_src_ring_read_index_from_ddr`.
    fn src_index_from_ddr<M: DescMem>(&self, mem: &mut M, ce_id: usize) -> Result<u32, CeError> {
        Ok(self.rri_word(mem, ce_id)? & DDR_RRI_MASK)
    }

    /// `ath10k_ce_src_ring_read_index_get`: from RRI memory for engines without interrupts, else the register.
    fn src_read_index<R: Regs, M: DescMem>(
        &self,
        regs: &mut R,
        mem: &mut M,
        ce_id: usize,
    ) -> Result<u32, CeError> {
        let p = self.pipe(ce_id).ok_or(CeError::BadPipe)?;
        if self.poll_from_memory || p.attr_flags & attr::DIS_INTR != 0 {
            self.src_index_from_ddr(mem, ce_id)
        } else {
            Ok(regs.read32(p.ctrl_addr + hw::CURRENT_SRRI))
        }
    }

    /// `ath10k_ce_dest_ring_read_index_get`.
    fn dest_read_index<R: Regs, M: DescMem>(
        &self,
        regs: &mut R,
        mem: &mut M,
        ce_id: usize,
    ) -> Result<u32, CeError> {
        let p = self.pipe(ce_id).ok_or(CeError::BadPipe)?;
        if p.attr_flags & attr::DIS_INTR != 0 {
            Ok((self.rri_word(mem, ce_id)? >> DDR_DRRI_SHIFT) & DDR_RRI_MASK)
        } else {
            Ok(regs.read32(p.ctrl_addr + hw::CURRENT_DRRI))
        }
    }

    // ---- allocation and initialisation -------------------------------------------------------------------------

    /// `ath10k_ce_alloc_pipe`, minus the DMA allocation: record the attributes and build the ring bookkeeping on
    /// memory the caller reserved ([`ring_mem_size`] bytes each). A ring is made iff the attribute asks for one.
    pub fn alloc_pipe(
        &mut self,
        ce_id: usize,
        attrs: &CeAttr,
        src: Option<RingPlacement>,
        dest: Option<RingPlacement>,
    ) -> Result<(), CeError> {
        if ce_id >= CE_COUNT {
            return Err(CeError::BadPipe);
        }
        let src_ring = match (attrs.src_nentries, src) {
            (0, _) => None,
            (n, Some(at)) => Some(Ring::new(ring_entries(n).ok_or(CeError::BadPipe)?, at)?),
            (_, None) => return Err(CeError::BadPipe),
        };
        let dest_ring = match (attrs.dest_nentries, dest) {
            (0, _) => None,
            (n, Some(at)) => Some(Ring::new(ring_entries(n).ok_or(CeError::BadPipe)?, at)?),
            (_, None) => return Err(CeError::BadPipe),
        };
        self.pipes[ce_id] = Some(Pipe {
            id: ce_id as u8,
            ctrl_addr: hw::ce_base(ce_id),
            attr_flags: attrs.flags,
            src_sz_max: attrs.src_sz_max,
            src_ring,
            dest_ring,
            has_send_cb: attrs.src_nentries != 0 && attrs.send_cb.is_some(),
            has_recv_cb: attrs.dest_nentries != 0 && attrs.recv_cb.is_some(),
        });
        Ok(())
    }

    /// `ath10k_ce_init_pipe`: zero the rings and program the device, source ring first.
    pub fn init_pipe<R: Regs, M: DescMem>(
        &mut self,
        regs: &mut R,
        mem: &mut M,
        ce_id: usize,
        attrs: &CeAttr,
    ) -> Result<(), CeError> {
        if attrs.src_nentries != 0 {
            self.init_src_ring(regs, mem, ce_id, attrs)?;
        }
        if attrs.dest_nentries != 0 {
            self.init_dest_ring(regs, mem, ce_id)?;
        }
        Ok(())
    }

    fn zero_ring<M: DescMem>(mem: &mut M, ring: &Ring) {
        let words = ring.nentries as usize * DESC_SIZE / 4;
        for i in 0..words {
            mem.write32(ring.host_base + 4 * i, 0);
        }
    }

    /// `ath10k_ce_init_src_ring`.
    fn init_src_ring<R: Regs, M: DescMem>(
        &mut self,
        regs: &mut R,
        mem: &mut M,
        ce_id: usize,
        attrs: &CeAttr,
    ) -> Result<(), CeError> {
        let ctrl = hw::ce_base(ce_id);
        {
            let p = self.pipe(ce_id).ok_or(CeError::BadPipe)?;
            let ring = p.src_ring.as_ref().ok_or(CeError::BadPipe)?;
            Self::zero_ring(mem, ring);
        }
        let read_index = self.src_read_index(regs, mem, ce_id)?;
        let write_index = regs.read32(ctrl + hw::SR_WR_INDEX);
        let p = self.pipe_mut(ce_id)?;
        let ring = p.src_ring.as_mut().ok_or(CeError::BadPipe)?;
        ring.sw_index = read_index & ring.mask;
        ring.hw_index = ring.sw_index;
        ring.write_index = write_index & ring.mask;
        let nentries = ring.nentries;
        let base = ring.bus_base;

        regs.write32(ctrl + hw::SR_BASE_ADDR_LO, base as u32);
        regs.write32(
            ctrl + hw::SR_BASE_ADDR_HI,
            ((base >> 32) as u32) & DESC_ADDR_HI_MASK,
        );
        regs.write32(ctrl + hw::SR_SIZE, nentries);
        // dmax
        rmw(regs, ctrl + hw::CTRL1, |v| {
            (v & !hw::CTRL1_DMAX_MASK)
                | set_field(attrs.src_sz_max, hw::CTRL1_DMAX_LSB, hw::CTRL1_DMAX_MASK)
        });
        // byte swap off
        rmw(regs, ctrl + hw::CTRL1, |v| {
            (v & !hw::CTRL1_SRC_RING_MASK)
                | set_field(0, hw::CTRL1_SRC_RING_LSB, hw::CTRL1_SRC_RING_MASK)
        });
        // low mark 0, high mark nentries
        rmw(regs, ctrl + hw::SRC_WM, |v| {
            (v & !hw::WM_LOW_MASK) | set_field(0, hw::WM_LOW_LSB, hw::WM_LOW_MASK)
        });
        rmw(regs, ctrl + hw::SRC_WM, |v| {
            (v & !hw::WM_HIGH_MASK) | set_field(nentries, hw::WM_HIGH_LSB, hw::WM_HIGH_MASK)
        });
        Ok(())
    }

    /// `ath10k_ce_init_dest_ring`.
    fn init_dest_ring<R: Regs, M: DescMem>(
        &mut self,
        regs: &mut R,
        mem: &mut M,
        ce_id: usize,
    ) -> Result<(), CeError> {
        let ctrl = hw::ce_base(ce_id);
        {
            let p = self.pipe(ce_id).ok_or(CeError::BadPipe)?;
            let ring = p.dest_ring.as_ref().ok_or(CeError::BadPipe)?;
            Self::zero_ring(mem, ring);
        }
        let read_index = self.dest_read_index(regs, mem, ce_id)?;
        let write_index = regs.read32(ctrl + hw::DST_WR_INDEX);
        let p = self.pipe_mut(ce_id)?;
        let ring = p.dest_ring.as_mut().ok_or(CeError::BadPipe)?;
        ring.sw_index = read_index & ring.mask;
        ring.write_index = write_index & ring.mask;
        let nentries = ring.nentries;
        let base = ring.bus_base;

        regs.write32(ctrl + hw::DR_BASE_ADDR_LO, base as u32);
        let hi = ((base >> 32) as u32) & DESC_ADDR_HI_MASK;
        rmw(regs, ctrl + hw::DR_BASE_ADDR_HI, |v| {
            (v & !DESC_ADDR_HI_MASK) | hi
        });
        regs.write32(ctrl + hw::DR_SIZE, nentries);
        rmw(regs, ctrl + hw::CTRL1, |v| {
            (v & !hw::CTRL1_DST_RING_MASK)
                | set_field(0, hw::CTRL1_DST_RING_LSB, hw::CTRL1_DST_RING_MASK)
        });
        rmw(regs, ctrl + hw::DST_WM, |v| {
            (v & !hw::WM_LOW_MASK) | set_field(0, hw::WM_LOW_LSB, hw::WM_LOW_MASK)
        });
        rmw(regs, ctrl + hw::DST_WM, |v| {
            (v & !hw::WM_HIGH_MASK) | set_field(nentries, hw::WM_HIGH_LSB, hw::WM_HIGH_MASK)
        });
        Ok(())
    }

    /// `ath10k_ce_deinit_pipe`: unprogram both rings (source first).
    pub fn deinit_pipe<R: Regs>(&mut self, regs: &mut R, ce_id: usize) -> Result<(), CeError> {
        if ce_id >= CE_COUNT {
            return Err(CeError::BadPipe);
        }
        let ctrl = hw::ce_base(ce_id);
        // src
        regs.write32(ctrl + hw::SR_BASE_ADDR_LO, 0);
        regs.write32(ctrl + hw::SR_BASE_ADDR_HI, 0);
        regs.write32(ctrl + hw::SR_SIZE, 0);
        rmw(regs, ctrl + hw::CTRL1, |v| {
            (v & !hw::CTRL1_DMAX_MASK) | set_field(0, hw::CTRL1_DMAX_LSB, hw::CTRL1_DMAX_MASK)
        });
        rmw(regs, ctrl + hw::SRC_WM, |v| {
            (v & !hw::WM_HIGH_MASK) | set_field(0, hw::WM_HIGH_LSB, hw::WM_HIGH_MASK)
        });
        // dest
        regs.write32(ctrl + hw::DR_BASE_ADDR_LO, 0);
        rmw(regs, ctrl + hw::DR_BASE_ADDR_HI, |v| v & !DESC_ADDR_HI_MASK);
        regs.write32(ctrl + hw::DR_SIZE, 0);
        rmw(regs, ctrl + hw::DST_WM, |v| {
            (v & !hw::WM_HIGH_MASK) | set_field(0, hw::WM_HIGH_LSB, hw::WM_HIGH_MASK)
        });
        Ok(())
    }

    // ---- source ring -------------------------------------------------------------------------------------------

    /// `ath10k_ce_num_free_src_entries`.
    pub fn num_free_src_entries(&self, ce_id: usize) -> Result<u32, CeError> {
        let ring = self
            .pipe(ce_id)
            .and_then(|p| p.src_ring.as_ref())
            .ok_or(CeError::BadPipe)?;
        Ok(ring.free_entries())
    }

    /// `_ath10k_ce_send_nolock_64`: queue one buffer. `flags` is `CE_SEND_FLAG_*`. `context` comes back from
    /// [`CopyEngine::completed_send_next`].
    ///
    /// The fullness check uses the target's read index from RRI memory (`rri_on_ddr`), not `sw_index`. A gather
    /// send (`SEND_FLAG_GATHER`) does not ring the doorbell; the last piece of the gather rings it. Quirks kept
    /// from the C: the caller's send flags are ORed into the second address word of the descriptor (so
    /// `SEND_FLAG_BYTE_SWAP` lands on address bit 32 and `SEND_FLAG_GATHER` on bit 16), and bit 31 is forced to
    /// the gather state.
    #[allow(clippy::too_many_arguments)]
    pub fn send<R: Regs, M: DescMem>(
        &mut self,
        regs: &mut R,
        mem: &mut M,
        ce_id: usize,
        context: u64,
        buffer: u64,
        nbytes: u32,
        transfer_id: u32,
        flags: u32,
    ) -> Result<(), CeError> {
        if self.crash_flush {
            return Err(CeError::Shutdown);
        }
        if nbytes > 0xffff {
            return Err(CeError::TooLong);
        }
        self.pipe(ce_id)
            .and_then(|p| p.src_ring.as_ref())
            .ok_or(CeError::BadPipe)?;
        let sw_index = self.src_index_from_ddr(mem, ce_id)?;
        let gather = flags & SEND_FLAG_GATHER != 0;
        let shadow = if gather {
            None
        } else {
            Some(hw::shadow_sr_wr_index(ce_id).ok_or(CeError::NoShadowReg)?)
        };
        let p = self.pipe_mut(ce_id)?;
        let ring = p.src_ring.as_mut().ok_or(CeError::BadPipe)?;
        let mask = ring.mask;
        let write_index = ring.write_index;
        if ring_delta(mask, write_index, sw_index.wrapping_sub(1)) == 0 {
            return Err(CeError::NoSpace);
        }
        let mut desc_flags: u32 = (transfer_id << hw::DESC_META_DATA_LSB) & hw::DESC_META_DATA_MASK;
        if gather {
            desc_flags |= u32::from(DESC_FLAGS_GATHER);
        }
        if flags & SEND_FLAG_BYTE_SWAP != 0 {
            desc_flags |= u32::from(DESC_FLAGS_BYTE_SWAP);
        }
        let hi_flags = flags | (((buffer >> 32) as u32) & DESC_ADDR_HI_MASK);
        let addr_hi = if gather {
            hi_flags | WCN3990_DESC_FLAGS_GATHER
        } else {
            hi_flags & !WCN3990_DESC_FLAGS_GATHER
        };
        let d = Desc64 {
            addr_lo: buffer as u32,
            addr_hi,
            nbytes: nbytes as u16,
            flags: desc_flags as u16,
            toeplitz_hash_result: 0,
        };
        let off = ring.desc_off(write_index);
        let b = d.to_le_bytes();
        for i in 0..4 {
            mem.write32(
                off + 4 * i,
                u32::from_le_bytes([b[4 * i], b[4 * i + 1], b[4 * i + 2], b[4 * i + 3]]),
            );
        }
        ring.contexts[(write_index & mask) as usize] = Some(context);
        let new_write = (write_index + 1) & mask;
        if let Some(shadow_off) = shadow {
            mem.barrier(Barrier::Release);
            regs.write32(shadow_off, new_write);
        }
        ring.write_index = new_write;
        Ok(())
    }

    /// `__ath10k_ce_send_revert`: take back the last send of a gather whose rest did not fit. Nothing happens if there
    /// is nothing to take back, or if the doorbell already told the device about it.
    pub fn send_revert<R: Regs>(&mut self, regs: &mut R, ce_id: usize) {
        let Some(p) = self.pipes.get_mut(ce_id).and_then(|p| p.as_mut()) else {
            return;
        };
        let ctrl = p.ctrl_addr;
        let Some(ring) = p.src_ring.as_mut() else {
            return;
        };
        if ring.write_index == ring.sw_index {
            return;
        }
        if ring.write_index == regs.read32(ctrl + hw::SR_WR_INDEX) {
            return;
        }
        ring.write_index = ring.write_index.wrapping_sub(1) & ring.mask;
        ring.contexts[ring.write_index as usize] = None;
    }

    /// Memory-only look at whether the source ring has a finished send (the read index the device mirrors into the
    /// RRI array), without consuming anything. For the HOST_IS clear before the drain.
    pub fn send_pending<M: DescMem>(&self, mem: &mut M, ce_id: usize) -> bool {
        let Some(ring) = self.pipe(ce_id).and_then(|p| p.src_ring.as_ref()) else {
            return false;
        };
        (self.src_index_from_ddr(mem, ce_id).unwrap_or(0) & ring.mask) != ring.sw_index
    }

    /// `_ath10k_ce_completed_send_next_nolock_64`: pop the oldest send the target has finished. `Ok(None)` is a
    /// completed send that had no context; [`CeError::NotReady`] means nothing more is done.
    ///
    /// The read index is fetched only when the cached one has been caught up with, then (rri_on_ddr) again for the
    /// comparison, which is what the C does.
    pub fn completed_send_next<R: Regs, M: DescMem>(
        &mut self,
        regs: &mut R,
        mem: &mut M,
        ce_id: usize,
    ) -> Result<Option<u64>, CeError> {
        let (mask, sw_index, hw_index) = {
            let ring = self
                .pipe(ce_id)
                .and_then(|p| p.src_ring.as_ref())
                .ok_or(CeError::BadPipe)?;
            (ring.mask, ring.sw_index, ring.hw_index)
        };
        if hw_index == sw_index {
            let ri = self.src_read_index(regs, mem, ce_id)?;
            if ri == 0xffff_ffff {
                return Err(CeError::NoDevice);
            }
            let ring = self
                .pipe_mut(ce_id)?
                .src_ring
                .as_mut()
                .ok_or(CeError::BadPipe)?;
            ring.hw_index = ri & mask;
        }
        let read_index = {
            // rri_on_ddr: read again, masked (see the module comment)
            self.src_read_index(regs, mem, ce_id)? & mask
        };
        if read_index == sw_index {
            return Err(CeError::NotReady);
        }
        let ring = self
            .pipe_mut(ce_id)?
            .src_ring
            .as_mut()
            .ok_or(CeError::BadPipe)?;
        let ctx = ring.contexts[sw_index as usize].take();
        let off = ring.desc_off(sw_index);
        mem.write16(off + 8, 0);
        ring.sw_index = (sw_index + 1) & mask;
        Ok(ctx)
    }

    // ---- destination ring --------------------------------------------------------------------------------------

    /// `__ath10k_ce_rx_num_free_bufs`.
    pub fn rx_num_free_bufs(&self, ce_id: usize) -> Result<u32, CeError> {
        let ring = self
            .pipe(ce_id)
            .and_then(|p| p.dest_ring.as_ref())
            .ok_or(CeError::BadPipe)?;
        Ok(ring.free_entries())
    }

    /// `__ath10k_ce_rx_post_buf_64`: hand the device an empty buffer. Fails with [`CeError::NoBuffers`] when the
    /// ring is full (one slot is always left empty).
    pub fn rx_post_buf<R: Regs, M: DescMem>(
        &mut self,
        regs: &mut R,
        mem: &mut M,
        ce_id: usize,
        context: u64,
        paddr: u64,
    ) -> Result<(), CeError> {
        let p = self.pipe_mut(ce_id)?;
        let ctrl = p.ctrl_addr;
        let ring = p.dest_ring.as_mut().ok_or(CeError::BadPipe)?;
        let mask = ring.mask;
        let write_index = ring.write_index;
        if ring_delta(mask, write_index, ring.sw_index.wrapping_sub(1)) == 0 {
            return Err(CeError::NoBuffers);
        }
        let off = ring.desc_off(write_index);
        mem.write64(off, paddr & DESC_ADDR_MASK);
        mem.write16(off + 8, 0);
        ring.contexts[(write_index & mask) as usize] = Some(context);
        let new_write = (write_index + 1) & mask;
        mem.barrier(Barrier::Release);
        regs.write32(ctrl + hw::DST_WR_INDEX, new_write);
        ring.write_index = new_write;
        Ok(())
    }

    /// `ath10k_ce_rx_update_write_idx`: advance the device's write index by `nentries` buffers that were posted
    /// without ringing the doorbell, keeping it one short of the read index so a full ring does not look empty.
    pub fn rx_update_write_idx<R: Regs>(
        &mut self,
        regs: &mut R,
        ce_id: usize,
        nentries: u32,
    ) -> Result<(), CeError> {
        let p = self.pipe_mut(ce_id)?;
        let ctrl = p.ctrl_addr;
        let ring = p.dest_ring.as_mut().ok_or(CeError::BadPipe)?;
        let mask = ring.mask;
        let cur = regs.read32(ctrl + hw::DST_WR_INDEX);
        let mut n = nentries;
        if (cur.wrapping_add(n) & mask) == ring.sw_index {
            n = n.wrapping_sub(1);
        }
        let write_index = ring.write_index.wrapping_add(n) & mask;
        regs.write32(ctrl + hw::DST_WR_INDEX, write_index);
        ring.write_index = write_index;
        Ok(())
    }

    /// Memory-only look at whether the destination ring's next descriptor is filled, without consuming it. For the
    /// HOST_IS clear before the drain.
    pub fn recv_pending<M: DescMem>(&self, mem: &mut M, ce_id: usize) -> bool {
        let Some(ring) = self.pipe(ce_id).and_then(|p| p.dest_ring.as_ref()) else {
            return false;
        };
        let off = ring.desc_off(ring.sw_index & ring.mask);
        mem.barrier(Barrier::Acquire);
        u32::from(mem.read16(off + 8)) != 0
    }

    /// `_ath10k_ce_completed_recv_next_nolock_64`: the next filled buffer, as `(context, nbytes)`.
    ///
    /// Completion is decided by the descriptor's `nbytes` being non-zero, not by the read index (a zero length
    /// means the device has not finished the descriptor, even if it advanced its index). The length comes straight
    /// from the device and can be up to 65535: the caller must bound it by the buffer it posted. Engine 5 keeps
    /// its context (it reuses buffers).
    pub fn completed_recv_next<M: DescMem>(
        &mut self,
        mem: &mut M,
        ce_id: usize,
    ) -> Result<(Option<u64>, u32), CeError> {
        let p = self.pipe_mut(ce_id)?;
        let is_ce5 = p.id == 5;
        let ring = p.dest_ring.as_mut().ok_or(CeError::BadPipe)?;
        let mask = ring.mask;
        let sw_index = ring.sw_index & mask;
        let off = ring.desc_off(sw_index);
        mem.barrier(Barrier::Acquire);
        let nbytes = u32::from(mem.read16(off + 8));
        if nbytes == 0 {
            return Err(CeError::NotReady);
        }
        mem.write16(off + 8, 0);
        let ctx = if is_ce5 {
            ring.contexts[sw_index as usize]
        } else {
            ring.contexts[sw_index as usize].take()
        };
        ring.sw_index = (sw_index + 1) & mask;
        Ok((ctx, nbytes))
    }

    // ---- interrupts --------------------------------------------------------------------------------------------

    /// `ath10k_ce_interrupt_summary`: one bit per engine with a pending interrupt.
    pub fn interrupt_summary<R: Regs>(&mut self, regs: &mut R) -> u32 {
        (regs.read32(hw::CE_WRAPPER_BASE + hw::CE_WRAPPER_INTERRUPT_SUMMARY)
            & hw::INTR_SUM_HOST_MSI_MASK)
            >> hw::INTR_SUM_HOST_MSI_LSB
    }

    /// `ath10k_ce_per_engine_service`: acknowledge the engine's interrupt (copy complete and the four
    /// watermarks) and report which callbacks to run, receive before send.
    pub fn per_engine_service<R: Regs>(
        &mut self,
        regs: &mut R,
        ce_id: usize,
    ) -> Result<Service, CeError> {
        let p = self.pipe(ce_id).ok_or(CeError::BadPipe)?;
        let (ctrl, recv, send) = (p.ctrl_addr, p.has_recv_cb, p.has_send_cb);
        regs.write32(ctrl + hw::HOST_IS, hw::WM_CC_MASK | hw::WM_MASK);
        Ok(Service {
            run_recv_cb: recv,
            run_send_cb: send,
        })
    }

    /// `ath10k_ce_per_engine_service_any`: service every engine the summary register flags, lowest first.
    pub fn service_any<R: Regs>(&mut self, regs: &mut R, mut f: impl FnMut(usize, Service)) {
        let mut summary = self.interrupt_summary(regs);
        let mut ce_id = 0;
        while summary != 0 && ce_id < CE_COUNT {
            if summary & (1 << ce_id) != 0 {
                summary &= !(1 << ce_id);
                if let Ok(s) = self.per_engine_service(regs, ce_id) {
                    f(ce_id, s);
                }
            }
            ce_id += 1;
        }
    }

    fn cc_intr<R: Regs>(regs: &mut R, ctrl: usize, enable: bool) {
        rmw(regs, ctrl + hw::HOST_IE, |v| {
            if enable {
                v | hw::HOST_IE_COPY_COMPLETE
            } else {
                v & !hw::HOST_IE_COPY_COMPLETE
            }
        });
    }

    fn wm_intr_disable<R: Regs>(regs: &mut R, ctrl: usize) {
        rmw(regs, ctrl + hw::HOST_IE, |v| v & !hw::WM_MASK);
    }

    /// `ath10k_ce_enable_interrupt` (`ath10k_ce_per_engine_handler_adjust`): unmask copy-complete when the engine
    /// has a callback and is not `DIS_INTR`, otherwise mask it; the watermark interrupts are always masked.
    /// Engines flagged `POLL` are left alone.
    pub fn enable_interrupt<R: Regs>(&mut self, regs: &mut R, ce_id: usize) -> Result<(), CeError> {
        let p = self.pipe(ce_id).ok_or(CeError::BadPipe)?;
        if p.attr_flags & attr::POLL != 0 {
            return Ok(());
        }
        let ctrl = p.ctrl_addr;
        let on = p.attr_flags & attr::DIS_INTR == 0 && (p.has_send_cb || p.has_recv_cb);
        Self::cc_intr(regs, ctrl, on);
        Self::wm_intr_disable(regs, ctrl);
        Ok(())
    }

    /// `ath10k_ce_disable_interrupt`: mask copy-complete, errors and watermarks.
    pub fn disable_interrupt<R: Regs>(
        &mut self,
        regs: &mut R,
        ce_id: usize,
    ) -> Result<(), CeError> {
        let p = self.pipe(ce_id).ok_or(CeError::BadPipe)?;
        if p.attr_flags & attr::POLL != 0 {
            return Ok(());
        }
        let ctrl = p.ctrl_addr;
        Self::cc_intr(regs, ctrl, false);
        rmw(regs, ctrl + hw::MISC_IE, |v| v & !hw::MISC_ERR_MASK);
        Self::wm_intr_disable(regs, ctrl);
        Ok(())
    }

    /// `ath10k_ce_enable_interrupts`: every engine, in order.
    pub fn enable_interrupts<R: Regs>(&mut self, regs: &mut R) {
        for id in 0..CE_COUNT {
            let _ = self.enable_interrupt(regs, id);
        }
    }

    /// `ath10k_ce_disable_interrupts`.
    pub fn disable_interrupts<R: Regs>(&mut self, regs: &mut R) {
        for id in 0..CE_COUNT {
            let _ = self.disable_interrupt(regs, id);
        }
    }
}
