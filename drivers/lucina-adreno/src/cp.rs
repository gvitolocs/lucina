// SPDX-License-Identifier: GPL-2.0-only
//! The command processor: `hw_init` and `a6xx_cp_init` of Linux's `a6xx_gpu.c` for the A618, its ring and fences. Runs with the GMU
//! holding GX on.

use lucina_qcom_soc::io::RegIo;

use crate::{Error, Hal, Result, err, gmu, pm4, poll, regs::*};

pub const RING_SIZE: usize = 0x8000;
const RING_WORDS: u32 = (RING_SIZE / 4) as u32;

pub const GMEM_SIZE: u64 = 512 * 1024;
pub const GMEM_BASE: u64 = 0x10_0000;
/// `adreno_gpu->uche_trap_base`.
const UCHE_TRAP_BASE_VALUE: u64 = 0x0001_ffff_ffff_f000;
/// `A6XX_INT_MASK` of `a6xx_gpu.c`: every fault and the timestamp bits Linux masks in.
const INT_CP_AHB_ERROR: u32 = 1 << 1;
const INT_ATB_ASYNCFIFO_OVERFLOW: u32 = 1 << 6;
const INT_CP_HW_ERROR: u32 = 1 << 9;
const INT_CP_IB2: u32 = 1 << 13;
const INT_CP_IB1: u32 = 1 << 14;
const INT_CP_RB: u32 = 1 << 15;
const INT_CP_CACHE_FLUSH_TS: u32 = 1 << 20;
const INT_ATB_BUS_OVERFLOW: u32 = 1 << 22;
const INT_RBBM_HANG_DETECT: u32 = 1 << 23;
const INT_UCHE_OOB: u32 = 1 << 24;
const INT_UCHE_TRAP: u32 = 1 << 25;
const INT_MASK: u32 = INT_CP_AHB_ERROR
    | INT_ATB_ASYNCFIFO_OVERFLOW
    | INT_CP_HW_ERROR
    | INT_CP_IB2
    | INT_CP_IB1
    | INT_CP_RB
    | INT_CP_CACHE_FLUSH_TS
    | INT_ATB_BUS_OVERFLOW
    | INT_RBBM_HANG_DETECT
    | INT_UCHE_OOB
    | INT_UCHE_TRAP;
const INT_FATAL: u32 = INT_MASK & !INT_CP_CACHE_FLUSH_TS;
/// `A6XX_RBBM_STATUS_CP_AHB_BUSY_CX_MASTER`, which may stay set while the rest is idle.
const STATUS_CP_AHB_BUSY: u32 = 1;
/// `CACHE_FLUSH_TS` of `vgt_event_type`.
const EVENT_CACHE_FLUSH_TS: u32 = 4;
/// `CP_EVENT_WRITE_0_IRQ`: the event raises `CP_CACHE_FLUSH_TS` in `RBBM_INT_0_STATUS` once its write is in memory.
const EVENT_WRITE_IRQ: u32 = 1 << 31;
/// The fence's bit of `RBBM_INT_0_STATUS`, as [`interrupt`] returns it.
pub const INT_FENCE: u32 = INT_CP_CACHE_FLUSH_TS;

/// Whether bits [`interrupt`] returned hold a fault (anything of Linux's `A6XX_INT_MASK` but the fence).
pub const fn is_fault(bits: u32) -> bool {
    bits & INT_FATAL != 0
}

/// The start of `a6xx_irq`, for the GPU's interrupt (GIC SPI 300, level high): reads `RBBM_INT_0_STATUS` and returns it. The
/// fence's bit is cleared (`RBBM_INT_CLEAR_CMD`), which drops the level. A fault bit is not cleared but masked
/// (`RBBM_INT_0_MASK` keeps only the fence), so the level drops too and the bit stays in the status for [`Ring::idle`] and
/// [`report`], which read it there: Linux logs and recovers in the handler; this driver reports when the batch fails, and the
/// next `hw_init` writes the whole mask again.
pub fn interrupt<W: RegIo>(w: &W) -> u32 {
    let status = rd(w, RBBM_INT_0_STATUS);
    if status & INT_CP_CACHE_FLUSH_TS != 0 {
        wr(w, RBBM_INT_CLEAR_CMD, INT_CP_CACHE_FLUSH_TS);
    }
    if status & INT_FATAL != 0 {
        wr(w, RBBM_INT_0_MASK, INT_CP_CACHE_FLUSH_TS);
    }
    status
}

/// Masks every GPU interrupt, before the GPU powers down (Linux disables the line around power changes).
pub fn mask_interrupts<W: RegIo>(w: &W) {
    wr(w, RBBM_INT_0_MASK, 0);
}

/// `a630_protect`.
const fn deny(reg: u32, len: u32) -> u32 {
    (1 << 31) | ((len & 0x3fff) << 18) | (reg & 0x3ffff)
}
const fn read_only(reg: u32, len: u32) -> u32 {
    ((len & 0x3fff) << 18) | (reg & 0x3ffff)
}
const CP_PROTECT: [u32; 32] = [
    read_only(0x00000, 0x04ff),
    read_only(0x00501, 0x0005),
    read_only(0x0050b, 0x02f4),
    deny(0x0050e, 0x0000),
    deny(0x00510, 0x0000),
    deny(0x00534, 0x0000),
    deny(0x00800, 0x0082),
    deny(0x008a0, 0x0008),
    deny(0x008ab, 0x0024),
    read_only(0x008de, 0x00ae),
    deny(0x00900, 0x004d),
    deny(0x0098d, 0x0272),
    deny(0x00e00, 0x0001),
    deny(0x00e03, 0x000c),
    deny(0x03c00, 0x00c3),
    read_only(0x03cc4, 0x1fff),
    deny(0x08630, 0x01cf),
    deny(0x08e00, 0x0000),
    deny(0x08e08, 0x0000),
    deny(0x08e50, 0x001f),
    deny(0x09624, 0x01db),
    deny(0x09e70, 0x0001),
    deny(0x09e78, 0x0187),
    deny(0x0a630, 0x01cf),
    deny(0x0ae02, 0x0000),
    deny(0x0ae50, 0x032f),
    deny(0x0b604, 0x0000),
    deny(0x0be02, 0x0001),
    deny(0x0be20, 0x17df),
    deny(0x0f000, 0x0bff),
    read_only(0x0fc00, 0x1fff),
    deny(0x11c00, 0x0000),
];

fn wr<W: RegIo>(w: &W, reg: u32, value: u32) {
    w.write32(reg * 4, value);
}
fn wr64<W: RegIo>(w: &W, reg: u32, value: u64) {
    wr(w, reg, value as u32);
    wr(w, reg + 1, (value >> 32) as u32);
}
fn rd<W: RegIo>(w: &W, reg: u32) -> u32 {
    w.read32(reg * 4)
}

/// `hw_init` up to the ring's registers, with the UBWC settings of `a6xx_set_ubwc_config` for the A618 (UBWC 2.0,
/// swizzle 0x6, `highest_bank_bit` 14: hbb 1, so hbb_lo 1, hbb_hi 0). The SQE is not started.
pub fn hw_init<W: RegIo, H: Hal>(w: &W, hal: &H, sqe_iova: u64, ring_iova: u64) {
    wr(w, GBIF_HALT, 0);
    let _ = rd(w, GBIF_HALT);
    wr(w, RBBM_GBIF_HALT, 0);
    let _ = rd(w, RBBM_GBIF_HALT);
    wr(w, RBBM_SECVID_TSB_CNTL, 0);
    wr64(w, RBBM_SECVID_TSB_TRUSTED_BASE, 0);
    wr(w, RBBM_SECVID_TSB_TRUSTED_SIZE, 0);
    for reg in [
        CP_ADDR_MODE_CNTL,
        VSC_ADDR_MODE_CNTL,
        GRAS_ADDR_MODE_CNTL,
        RB_ADDR_MODE_CNTL,
        PC_ADDR_MODE_CNTL,
        HLSQ_ADDR_MODE_CNTL,
        VFD_ADDR_MODE_CNTL,
        VPC_ADDR_MODE_CNTL,
        UCHE_ADDR_MODE_CNTL,
        SP_ADDR_MODE_CNTL,
        TPL1_ADDR_MODE_CNTL,
        RBBM_SECVID_TSB_ADDR_MODE_CNTL,
    ] {
        wr(w, reg, 1);
    }
    wr(w, RBBM_VBIF_CLIENT_QOS_CNTL, 3);
    wr(w, RBBM_PERFCTR_GPU_BUSY_MASKED, u32::MAX);
    wr64(w, UCHE_WRITE_RANGE_MAX, UCHE_TRAP_BASE_VALUE + 0xfc0);
    wr64(w, UCHE_TRAP_BASE, UCHE_TRAP_BASE_VALUE);
    wr64(w, UCHE_WRITE_THRU_BASE, UCHE_TRAP_BASE_VALUE);
    wr64(w, UCHE_GMEM_RANGE_MIN, GMEM_BASE);
    wr64(w, UCHE_GMEM_RANGE_MAX, GMEM_BASE + GMEM_SIZE - 1);
    wr(w, UCHE_FILTER_CNTL, 0x804);
    wr(w, UCHE_CACHE_WAYS, 4);
    wr(w, CP_ROQ_THRESHOLDS_2, 0x0100_00c0);
    wr(w, CP_ROQ_THRESHOLDS_1, 0x8040_362c);
    wr(w, CP_MEM_POOL_SIZE, 128);
    wr(w, PC_DBG_ECO_CNTL, 0x0018_0000);
    wr(w, CP_AHB_CNTL, 1);
    wr(w, RBBM_PERFCTR_CNTL, 1);
    wr(w, CP_PERFCTR_CP_SEL_0, 0);
    wr(w, RBBM_INTERFACE_HANG_INT_CNTL, (1 << 30) | 0x1f_ffff);
    wr(w, UCHE_CLIENT_PF, (1 << 7) | 1);
    wr(w, GMU_AO_GPU_CX_BUSY_MASK, 0xff00_0000);
    wr(
        w,
        GMU_POWER_COUNTER_SELECT_0,
        (rd(w, GMU_POWER_COUNTER_SELECT_0) & !0xff) | (1 << 5),
    );
    wr(w, GMU_POWER_COUNTER_ENABLE, 1);
    wr(w, CP_PROTECT_CNTL, 1 | (1 << 1) | (1 << 3));
    for (index, value) in CP_PROTECT.iter().enumerate() {
        wr(w, CP_PROTECT_BASE + index as u32, *value);
    }
    // `a6xx_set_ubwc_config` for the A618: level 2 swizzling on, hbb_lo 1, UBWC 2.0, no macrotile.
    wr(w, RB_NC_MODE_CNTL, 0x2);
    wr(w, TPL1_NC_MODE_CNTL, 0x2);
    wr(w, SP_NC_MODE_CNTL, 0x2);
    wr(w, UCHE_MODE_CNTL, 0x20_0000);
    wr(w, RBBM_NC_MODE_CNTL, 0);
    wr(w, RBBM_INT_CLEAR_CMD, u32::MAX);
    wr(w, RBBM_INT_0_MASK, INT_MASK);
    wr64(w, CP_SQE_INSTR_BASE, sqe_iova);
    wr64(w, CP_RB_BASE, ring_iova);
    // `MSM_GPU_RB_CNTL_DEFAULT | AXXX_CP_RB_CNTL_NO_UPDATE`: 32 KiB, blocks of 32 bytes, no shadow.
    wr(w, CP_RB_CNTL, 12 | (2 << 8) | (1 << 27));
    wr(w, CP_RB_WPTR, 0);
    hal.barrier();
}

/// The end of Linux's `hw_init`, after the CP left secure mode: "Tell the GMU that we are done touching the GPU and it can
/// start power management" (`OOB_GPU_SET` cleared), then, legacy, "Take the GMU out of its special boot mode"
/// (`OOB_BOOT_SLUMBER`). At the A618's idle level (`GMU_IDLE_STATE_ACTIVE`) the GMU still keeps GX on until a slumber.
pub fn hw_init_done<W: RegIo, H: Hal>(w: &W, hal: &H) {
    gmu::clear_oob(w, gmu::OOB_GPU_SET);
    gmu::clear_oob(w, gmu::OOB_BOOT_SLUMBER);
    hal.barrier();
}

/// `RBBM_SW_RESET_CMD` of `a6xx_gpu_sw_reset`: a read back for ordering, then the line held for 100 us when asserted.
pub fn sw_reset<W: RegIo, H: Hal>(w: &W, hal: &H, assert: bool) {
    wr(w, RBBM_SW_RESET_CMD, assert as u32);
    let _ = rd(w, RBBM_SW_RESET_CMD);
    hal.delay_us(1);
    if assert {
        hal.delay_us(100);
    }
}

/// `GBIF_CLIENT_HALT_MASK` and `GBIF_ARB_HALT_MASK` of `a6xx_gpu.c`.
const GBIF_CLIENT_HALT: u32 = 1 << 0;
const GBIF_ARB_HALT: u32 = 1 << 1;
/// `ADRENO_IDLE_TIMEOUT`, `spin_until`'s limit.
const SPIN_US: u64 = 1_000_000;

/// `a6xx_bus_clear_pending_transactions` for a GPU with a GBIF: with `gx_off`, the GX side of the GBIF halted first, then
/// new client requests, then all AXI requests, each waited for, and the halt cleared again.
pub fn bus_halt<W: RegIo, H: Hal>(w: &W, hal: &H, gx_off: bool) -> Result<()> {
    if gx_off {
        wr(w, RBBM_GBIF_HALT, 1);
        poll(hal, "GBIF: the GX side does not halt", SPIN_US, 1, || rd(w, RBBM_GBIF_HALT_ACK), |v| v & 1 != 0)?;
    }
    wr(w, GBIF_HALT, GBIF_CLIENT_HALT);
    poll(hal, "GBIF: client requests do not halt", SPIN_US, 1, || rd(w, GBIF_HALT_ACK), |v| v & GBIF_CLIENT_HALT == GBIF_CLIENT_HALT)?;
    wr(w, GBIF_HALT, GBIF_ARB_HALT);
    poll(hal, "GBIF: AXI requests do not halt", SPIN_US, 1, || rd(w, GBIF_HALT_ACK), |v| v & GBIF_ARB_HALT == GBIF_ARB_HALT)?;
    wr(w, GBIF_HALT, 0);
    Ok(())
}

/// `fenced_write` of `a6xx_gpu.c`: a write the GMU's AHB fence may drop while it power-collapses, checked in
/// `GMU_AHB_FENCE_STATUS` (`mask` names the WRITEDROPPED bits) and written again, for 1 ms and then another 1 ms.
pub fn fenced_write<W: RegIo, H: Hal>(w: &W, hal: &H, reg: u32, value: u32, mask: u32) -> Result<()> {
    // `fence_status_check`: a dropped write is written again after 10 us.
    let wait = || {
        let start = hal.now_us();
        loop {
            hal.barrier();
            if rd(w, GMU_AHB_FENCE_STATUS) & mask == 0 {
                return true;
            }
            if hal.now_us().saturating_sub(start) >= 1000 {
                return false;
            }
            hal.delay_us(10);
            wr(w, reg, value);
        }
    };
    wr(w, reg, value);
    if wait() {
        return Ok(());
    }
    wr(w, reg, value);
    if wait() {
        hal.log(format_args!("gpu: cp: delay in fenced register write ({:#x})", reg));
        return Ok(());
    }
    err("fenced register write failed", reg)
}

/// The ring, in uncached memory that the GPU reads.
pub struct Ring<M> {
    mem: M,
    wptr: u32,
}

impl<M: RegIo> Ring<M> {
    pub fn new(mem: M) -> Self {
        for index in 0..RING_WORDS {
            mem.write32(index * 4, 0);
        }
        Self { mem, wptr: 0 }
    }

    pub fn wptr(&self) -> u32 {
        self.wptr
    }

    /// How many words fit before the reader, given its position.
    fn space(&self, rptr: u32) -> u32 {
        (rptr + RING_WORDS - self.wptr - 1) % RING_WORDS
    }

    /// [`Self::space`] for a caller that waits for room on the fence interrupt before [`Self::submit`], which would poll.
    pub fn room(&self, rptr: u32) -> u32 {
        self.space(rptr % RING_WORDS)
    }

    /// Writes `words` and tells the CP. Waits for room if the ring is full.
    pub fn submit<W: RegIo, H: Hal>(&mut self, w: &W, hal: &H, words: &[u32]) -> Result<()> {
        if words.len() as u32 >= RING_WORDS {
            return err(
                "ring: a submission larger than the ring",
                words.len() as u32,
            );
        }
        let need = words.len() as u32;
        poll(
            hal,
            "ring: no room",
            1_000_000,
            10,
            || rd(w, CP_RB_RPTR),
            |rptr| self.space(rptr) >= need,
        )?;
        for word in words {
            self.mem.write32(self.wptr * 4, *word);
            self.wptr = (self.wptr + 1) % RING_WORDS;
        }
        hal.barrier();
        // `a6xx_flush`: the write pointer through the GMU's fence.
        fenced_write(w, hal, CP_RB_WPTR, self.wptr, 1)?;
        hal.barrier();
        Ok(())
    }

    /// After a power collapse the CP starts again from the ring's first word (`adreno_hw_init`: `ring->cur = ring->start`);
    /// [`hw_init`] writes `CP_RB_WPTR` 0 to match. What the ring held is not read again.
    pub fn reset(&mut self) {
        self.wptr = 0;
    }

    /// `a6xx_idle`: the CP has read everything and the GPU has nothing left to do.
    pub fn idle<W: RegIo, H: Hal>(
        &self,
        w: &W,
        hal: &H,
        what: &'static str,
        timeout_us: u64,
    ) -> Result<()> {
        let start = hal.now_us();
        loop {
            let status = rd(w, RBBM_STATUS);
            let int = rd(w, RBBM_INT_0_STATUS);
            if int & INT_FATAL != 0 {
                return Err(Error { what, value: int });
            }
            if rd(w, CP_RB_RPTR) == self.wptr && status & !STATUS_CP_AHB_BUSY == 0 {
                return Ok(());
            }
            if hal.now_us().saturating_sub(start) >= timeout_us {
                return Err(Error {
                    what,
                    value: status,
                });
            }
            hal.delay_us(10);
        }
    }
}

/// `a6xx_cp_init`.
pub const fn me_init() -> [u32; 9] {
    [
        pm4::pkt7(pm4::CP_ME_INIT, 8),
        0x0000_002f,
        // "Enable multiple hardware contexts".
        0x0000_0003,
        // "Enable error detection".
        0x2000_0000,
        0,
        0,
        0,
        0,
        0,
    ]
}

/// What Linux sends after the zap shader is in: the CP leaves secure mode, and only then may the GPU reach ordinary memory.
pub const fn leave_secure_mode() -> [u32; 2] {
    [pm4::pkt7(pm4::CP_SET_SECURE_MODE, 1), 0]
}

/// A fence: the sequence number goes to `CP_SCRATCH_2` when the CP gets here and to memory at `iova` when everything before it
/// has reached memory, then the GPU interrupts (`a6xx_submit`: `CP_EVENT_WRITE_0_EVENT(CACHE_FLUSH_TS) | CP_EVENT_WRITE_0_IRQ`).
pub const fn fence(iova: u64, sequence: u32) -> [u32; 7] {
    [
        pm4::pkt4(CP_SCRATCH_2, 1),
        sequence,
        pm4::pkt7(pm4::CP_EVENT_WRITE, 4),
        EVENT_CACHE_FLUSH_TS | EVENT_WRITE_IRQ,
        iova as u32,
        (iova >> 32) as u32,
        sequence,
    ]
}

/// The context registers Mesa's `fd6_emit_restore` gives an A618 (`freedreno_devices.py`), which Linux leaves to userspace. This driver
/// has no userspace drawing, so it sets them once, as Scarlet does. Dwords.
const CONTEXT: [(u32, u32); 22] = [
    (0x0e12, 0x0000_0001), // UCHE_UNKNOWN_0E12
    (0x80a0, 0x0000_0002), // GRAS_SC_CNTL
    (0x8600, 0x0000_0880), // GRAS_DBG_ECO_CNTL
    (0x8811, 0x0000_0010), // RB_MODE_CNTL
    (0x8e01, 0x0000_0001), // RB_RBP_CNTL
    (0x8e04, 0x0410_0000), // RB_DBG_ECO_CNTL
    (0x9600, 0x0000_0000), // VPC_DBG_ECO_CNTL
    (0x9804, 0x0000_001f), // PC_MODE_CNTL
    (0x9805, 0x0000_0000), // PC_POWER_CNTL
    (0xa009, 0x0000_0003), // VFD_MODE_CNTL
    (0xa0f8, 0x0000_0000), // VFD_POWER_CNTL
    (0xab20, 0x0000_0000), // SP_GFX_USIZE
    (0xae00, 0x0000_0000), // SP_DBG_ECO_CNTL
    (0xae03, 0x0000_0430), // SP_CHICKEN_BITS
    (0xae0f, 0x0000_003f), // SP_PERFCTR_SHADER_MASK
    (0xb600, 0x0010_8000), // TPL1_DBG_ECO_CNTL
    (0xb605, 0x0000_0044), // TPL1_UNKNOWN_B605
    (0xbb11, 0x0000_0000), // HLSQ_SHARED_CONSTS
    (0xbe00, 0x0000_0080), // HLSQ_UNKNOWN_BE00
    (0xbe01, 0x0000_0000), // HLSQ_UNKNOWN_BE01
    (0xbe04, 0x0008_0000), // HLSQ_DBG_ECO_CNTL
    (crate::a2d::RB_CCU_CNTL, crate::a2d::CCU_BYPASS),
];

/// Sets [`CONTEXT`]. Runs with the CP up.
pub fn context_init<W: RegIo>(w: &W) {
    for (reg, value) in CONTEXT {
        wr(w, reg, value);
    }
}

/// [`CONTEXT`] again, as ring packets: a program's command buffers share these registers with the kernel's batches, and
/// the ring writes them back before the kernel's next batch (patch 1171).
pub fn context_words(words: &mut alloc::vec::Vec<u32>) {
    for (reg, value) in CONTEXT {
        words.push(pm4::pkt4(reg, 1));
        words.push(value);
    }
}

/// `a6xx_ucode_check_version` for `a630_sqe.fw`: the firmware must carry the whereami fix, either as a patched build
/// (low nibble 0xa and patchlevel >= 1) or as version >= 0x190. Refuses the load otherwise.
pub fn sqe_version_ok(buf: &[u32]) -> Result<()> {
    if buf.len() < 3 {
        return err("SQE firmware too short", buf.len() as u32);
    }
    if ((buf[0] & 0xf) == 0xa && (buf[2] & 0xf) >= 1) || (buf[0] & 0xfff) >= 0x190 {
        Ok(())
    } else {
        err("SQE firmware too old, need at least 0x190", buf[0] & 0xfff)
    }
}

/// Starts the SQE.
pub fn start<W: RegIo, H: Hal>(w: &W, hal: &H) {
    wr(w, CP_SQE_CNTL, 1);
    hal.barrier();
}

/// What the GPU shows, for each failure.
pub fn report<W: RegIo, H: Hal>(w: &W, hal: &H) {
    let int = rd(w, RBBM_INT_0_STATUS);
    let cp = rd(w, CP_INTERRUPT_STATUS);
    hal.log(format_args!(
        "gpu: cp: rptr {:#x} wptr {:#x} status {:#x} status3 {:#x} int {:#x} cp-int {:#x} hw-fault {:#x} protect {:#x} scratch2 {:#x}",
        rd(w, CP_RB_RPTR),
        rd(w, CP_RB_WPTR),
        rd(w, RBBM_STATUS),
        rd(w, RBBM_STATUS3),
        int,
        cp,
        rd(w, CP_HW_FAULT),
        rd(w, CP_PROTECT_STATUS),
        rd(w, CP_SCRATCH_2),
    ));
    wr(w, CP_SQE_STAT_ADDR, 1);
    hal.log(format_args!(
        "gpu: cp: last opcode word {:#x}, ib1 {:#x} left {:#x}",
        rd(w, CP_SQE_STAT_DATA),
        u64::from(rd(w, CP_IB1_BASE)) | (u64::from(rd(w, CP_IB1_BASE + 1)) << 32),
        rd(w, CP_IB1_REM_SIZE),
    ));
}

/// Where the CP reads in its IB1 (`CP_IB1_BASE` low word) and how much is left (`CP_IB1_REM_SIZE`): what Linux's
/// `a6xx_progress` compares between hangcheck looks.
pub fn ib1_base<W: RegIo>(w: &W) -> u32 {
    rd(w, CP_IB1_BASE)
}
pub fn ib1_rem<W: RegIo>(w: &W) -> u32 {
    rd(w, CP_IB1_REM_SIZE)
}

/// Where the CP reads the ring (`CP_RB_RPTR`), in words.
pub fn rptr<W: RegIo>(w: &W) -> u32 {
    rd(w, CP_RB_RPTR)
}

/// Sequence-free: whether the scratch register has reached `sequence`.
pub fn scratch<W: RegIo>(w: &W) -> u32 {
    rd(w, CP_SCRATCH_2)
}

/// Stops the CP and the GPU's way to memory, for a jump to another kernel: `a6xx_gmu`'s `a6xx_bus_clear_pending_transactions`
/// without the waits, because a kernel on its way out cannot act on a timeout anyway.
pub fn stop<W: RegIo, H: Hal>(w: &W, hal: &H) {
    wr(w, CP_SQE_CNTL, 0);
    wr(w, RBBM_INT_0_MASK, 0);
    wr(w, RBBM_GBIF_HALT, 1);
    hal.delay_us(100);
    wr(w, GBIF_HALT, 3);
    hal.delay_us(100);
    hal.barrier();
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
    fn hw_init_writes_the_a618_ubwc_registers_before_the_irq_mask() {
        let w = Trace(RefCell::new(StdVec::new()));
        hw_init(&w, &Hal { now: Cell::new(0) }, 0x6000_0000, 0x6000_0000);
        let trace = w.0.borrow();
        let at = |reg: u32| {
            trace
                .iter()
                .position(|(o, _)| *o == reg * 4)
                .unwrap_or(usize::MAX)
        };
        let mask_at = at(RBBM_INT_0_MASK);
        for (reg, value) in [
            (RB_NC_MODE_CNTL, 0x2),
            (TPL1_NC_MODE_CNTL, 0x2),
            (SP_NC_MODE_CNTL, 0x2),
            (UCHE_MODE_CNTL, 0x20_0000),
            (RBBM_NC_MODE_CNTL, 0),
        ] {
            let pos = at(reg);
            assert!(pos < mask_at, "UBWC register {:#x} after the mask", reg);
            assert_eq!(trace[pos].1, value);
        }
    }

    #[test]
    fn the_irq_mask_is_linuxs_a6xx_int_mask() {
        let w = Trace(RefCell::new(StdVec::new()));
        hw_init(&w, &Hal { now: Cell::new(0) }, 0, 0);
        let trace = w.0.borrow();
        let mask = trace
            .iter()
            .find(|(o, _)| *o == RBBM_INT_0_MASK * 4)
            .map(|(_, v)| *v)
            .unwrap();
        assert_eq!(
            mask,
            (1 << 1) | (1 << 6) | (1 << 9) | (1 << 13) | (1 << 14) | (1 << 15) | (1 << 20) | (1 << 22)
                | (1 << 23) | (1 << 24) | (1 << 25)
        );
    }

    #[test]
    fn hw_init_leaves_the_oobs_and_the_end_clears_gpu_set_then_boot_slumber() {
        let w = Trace(RefCell::new(StdVec::new()));
        let hal = Hal { now: Cell::new(0) };
        hw_init(&w, &hal, 0, 0);
        assert!(!w.0.borrow().iter().any(|(o, _)| *o == GMU_HOST2GMU_INTR_SET * 4));
        hw_init_done(&w, &hal);
        let trace = w.0.borrow();
        let oob: StdVec<u32> = trace
            .iter()
            .filter(|(o, _)| *o == GMU_HOST2GMU_INTR_SET * 4)
            .map(|(_, v)| *v)
            .collect();
        // `a6xx_gmu_clear_oob(GPU_SET)` (clear bit 24), then `BOOT_SLUMBER` (bit 30), as at the end of `hw_init`.
        assert_eq!(oob, [1 << 24, 1 << 30]);
    }

    #[test]
    fn the_sqe_firmware_version_is_checked() {
        // This laptop's file: 0x207 >= 0x190.
        assert!(sqe_version_ok(&[0x016e_e207, 0, 0]).is_ok());
        // A patched build: low nibble 0xa, patchlevel >= 1.
        assert!(sqe_version_ok(&[0x0000_000a, 0, 0x0000_0001]).is_ok());
        assert!(sqe_version_ok(&[0x0000_000a, 0, 0x0000_0000]).is_err());
        // Too old.
        assert!(sqe_version_ok(&[0x0000_018f, 0, 0]).is_err());
        assert!(sqe_version_ok(&[0x0000_0000]).is_err());
    }

    #[test]
    fn the_protect_table_is_linuxs_length_and_ends_with_a_deny() {
        assert_eq!(CP_PROTECT.len(), 32);
        assert_eq!(CP_PROTECT[31], (1 << 31) | 0x11c00);
    }

    #[test]
    fn the_interrupt_clears_the_fence_and_masks_a_fault() {
        let w = Trace(RefCell::new(StdVec::new()));
        w.write32(RBBM_INT_0_STATUS * 4, INT_CP_CACHE_FLUSH_TS);
        assert_eq!(interrupt(&w), INT_CP_CACHE_FLUSH_TS);
        assert_eq!(w.read32(RBBM_INT_CLEAR_CMD * 4), INT_CP_CACHE_FLUSH_TS);
        assert!(w.0.borrow().iter().all(|(o, _)| *o != RBBM_INT_0_MASK * 4));
        assert!(!is_fault(INT_CP_CACHE_FLUSH_TS));

        let w = Trace(RefCell::new(StdVec::new()));
        w.write32(RBBM_INT_0_STATUS * 4, INT_RBBM_HANG_DETECT);
        assert_eq!(interrupt(&w), INT_RBBM_HANG_DETECT);
        assert_eq!(w.read32(RBBM_INT_0_MASK * 4), INT_CP_CACHE_FLUSH_TS);
        assert!(w.0.borrow().iter().all(|(o, _)| *o != RBBM_INT_CLEAR_CMD * 4));
        assert!(is_fault(INT_RBBM_HANG_DETECT));
    }

    #[test]
    fn a_fence_carries_its_address_and_sequence() {
        let words = fence(0x1_2345_6780, 9);
        assert_eq!(words[1], 9);
        // CACHE_FLUSH_TS with CP_EVENT_WRITE_0_IRQ, as a6xx_submit.
        assert_eq!(words[3], 4 | (1 << 31));
        assert_eq!(words[4], 0x2345_6780);
        assert_eq!(words[5], 1);
        assert_eq!(words[6], 9);
    }
}
