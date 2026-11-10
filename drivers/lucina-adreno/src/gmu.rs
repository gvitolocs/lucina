// SPDX-License-Identifier: GPL-2.0-only
//! The GMU: a Cortex-M3 with its own firmware that owns the GX power rail and votes for it. `a6xx_gmu.c` of Linux, the legacy
//! path, which is the one the A618 takes (`gmu->legacy`). Register writes are in Linux's order.

use lucina_qcom_soc::io::RegIo;

use crate::{Hal, Result, err, hfi, poll, regs::*};

/// The GMU's address space: Linux allocates its memory from here (`a6xx_gmu_memory_alloc`, `gmu->uncached_iova_base`).
pub const IOVA_BASE: u32 = 0x6000_0000;
pub const DUMMY_SIZE: u32 = 0x1000;
pub const DEBUG_IOVA: u32 = IOVA_BASE + DUMMY_SIZE;
pub const DEBUG_SIZE: u32 = 0x4000;
pub const LOG_IOVA: u32 = DEBUG_IOVA + DEBUG_SIZE;
pub const LOG_SIZE: u32 = 0x4000;
pub const HFI_IOVA: u32 = LOG_IOVA + LOG_SIZE;
/// Dummy, debug, log and queues, contiguous from [`IOVA_BASE`].
pub const MEMORY_SIZE: u32 = DUMMY_SIZE + DEBUG_SIZE + LOG_SIZE + hfi::SIZE as u32;
pub const FIRMWARE_MAX: usize = 0x8000;

/// `0x06010800` as Linux's catalog names the chip, in the GMU's own layout.
pub const fn gmu_chip_id(chip: u32) -> u32 {
    (chip & 0xffff_0000) | ((chip << 4) & 0xf000) | ((chip << 8) & 0x0f00)
}
pub const CHIP_ID: u32 = 0x0601_0800;

/// `a6xx_gmu_oob_bits`, legacy.
#[derive(Clone, Copy)]
pub struct Oob {
    set: u32,
    ack: u32,
    clear: u32,
}
pub const OOB_GPU_SET: Oob = Oob {
    set: 16,
    ack: 24,
    clear: 24,
};
pub const OOB_BOOT_SLUMBER: Oob = Oob {
    set: 22,
    ack: 30,
    clear: 30,
};
pub const OOB_DCVS: Oob = Oob {
    set: 23,
    ack: 31,
    clear: 31,
};

fn rd<W: RegIo>(w: &W, reg: u32) -> u32 {
    w.read32(reg * 4)
}
fn wr<W: RegIo>(w: &W, reg: u32, value: u32) {
    w.write32(reg * 4, value);
}
fn rscc<W: RegIo>(w: &W, reg: u32, value: u32) {
    w.write32(RSCC_OFFSET + reg * 4, value);
}

fn pdc_cmd<P: RegIo>(pdc: &P, msgid: u32, addr: u32, data: u32, index: u32, a: u32, d: u32) {
    pdc.write32((msgid + index * 4) * 4, 0x10108);
    pdc.write32((addr + index * 4) * 4, a);
    pdc.write32((data + index * 4) * 4, d);
}

/// `a6xx_gmu_rpmh_init`: the RSC and PDC sequences for the A618. MX, CX and XO are the addresses the laptop's command DB also
/// gives: `mx.lvl` 0x30010, `cx.lvl` 0x30000, `xo.lvl` 0x30090.
pub fn rpmh_init<W: RegIo, P: RegIo, S: RegIo, H: Hal>(w: &W, pdc: &P, seq: &S, hal: &H) {
    rscc(w, RSCC_RSC_STATUS0_DRV0, 1 << 24);
    rscc(w, RSCC_PDC_SLAVE_ID_DRV0, 1);
    rscc(w, RSCC_HIDDEN_TCS_CMD0_DATA, 0);
    rscc(w, RSCC_HIDDEN_TCS_CMD0_ADDR, 0);
    rscc(w, RSCC_HIDDEN_TCS_CMD0_DATA + 2, 0);
    rscc(w, RSCC_HIDDEN_TCS_CMD0_ADDR + 2, 0);
    rscc(w, RSCC_HIDDEN_TCS_CMD0_DATA + 4, 0x8000_0000);
    rscc(w, RSCC_HIDDEN_TCS_CMD0_ADDR + 4, 0);
    rscc(w, RSCC_OVERRIDE_START_ADDR, 0);
    rscc(w, RSCC_PDC_SEQ_START_ADDR, 0x4520);
    rscc(w, RSCC_PDC_MATCH_VALUE_LO, 0x4510);
    rscc(w, RSCC_PDC_MATCH_VALUE_HI, 0x4514);
    for (index, word) in [
        0xa7a5_06a0,
        0xa1e6_a6e7,
        0xa2e0_81e1,
        0xe9a9_82e2,
        0x0020_e8a8,
    ]
    .iter()
    .enumerate()
    {
        rscc(w, RSCC_SEQ_MEM_0_DRV0 + index as u32, *word);
    }
    for (index, word) in [
        0xfebe_a1e1,
        0xa5a4_a3a2,
        0x8382_a6e0,
        0xbce3_e284,
        0x0020_81fc,
    ]
    .iter()
    .enumerate()
    {
        seq.write32((PDC_GPU_SEQ_MEM_0 + index as u32) * 4, *word);
    }
    pdc.write32(PDC_GPU_TCS1_CMD_ENABLE_BANK * 4, 7);
    pdc.write32(PDC_GPU_TCS1_CMD_WAIT_FOR_CMPL_BANK * 4, 0);
    pdc.write32(PDC_GPU_TCS1_CONTROL * 4, 0);
    let t1 = (
        PDC_GPU_TCS1_CMD0_MSGID,
        PDC_GPU_TCS1_CMD0_ADDR,
        PDC_GPU_TCS1_CMD0_DATA,
    );
    pdc_cmd(pdc, t1.0, t1.1, t1.2, 0, 0x30010, 1);
    pdc_cmd(pdc, t1.0, t1.1, t1.2, 1, 0x30000, 0);
    pdc_cmd(pdc, t1.0, t1.1, t1.2, 2, 0x30090, 0);
    pdc.write32(PDC_GPU_TCS3_CMD_ENABLE_BANK * 4, 7);
    pdc.write32(PDC_GPU_TCS3_CMD_WAIT_FOR_CMPL_BANK * 4, 0);
    pdc.write32(PDC_GPU_TCS3_CONTROL * 4, 0);
    let t3 = (
        PDC_GPU_TCS3_CMD0_MSGID,
        PDC_GPU_TCS3_CMD0_ADDR,
        PDC_GPU_TCS3_CMD0_DATA,
    );
    pdc_cmd(pdc, t3.0, t3.1, t3.2, 0, 0x30010, 2);
    pdc_cmd(pdc, t3.0, t3.1, t3.2, 1, 0x30000, 2);
    pdc_cmd(pdc, t3.0, t3.1, t3.2, 2, 0x30090, 3);
    pdc.write32(PDC_GPU_SEQ_START_ADDR * 4, 0);
    pdc.write32(PDC_GPU_ENABLE_PDC * 4, 0x8000_0001);
    hal.barrier();
}

/// `a6xx_gmu_set_oob`.
pub fn set_oob<W: RegIo, H: Hal>(w: &W, hal: &H, oob: Oob, what: &'static str) -> Result<()> {
    wr(w, GMU_HOST2GMU_INTR_SET, 1 << oob.set);
    hal.barrier();
    let result = poll(
        hal,
        what,
        10_000,
        100,
        || rd(w, GMU_GMU2HOST_INTR_INFO),
        |v| v & (1 << oob.ack) != 0,
    );
    result?;
    wr(w, GMU_GMU2HOST_INTR_CLR, 1 << oob.ack);
    Ok(())
}

/// `a6xx_gmu_clear_oob`.
pub fn clear_oob<W: RegIo>(w: &W, oob: Oob) {
    wr(w, GMU_HOST2GMU_INTR_SET, 1 << oob.clear);
}

/// `a6xx_gmu_power_config`, at the idle level `GMU_IDLE_STATE_ACTIVE`: GX stays up while this kernel holds it.
fn power_config<W: RegIo>(w: &W) {
    wr(w, GMU_SYS_BUS_CONFIG, 1);
    wr(w, GMU_ICACHE_CONFIG, 1);
    wr(w, GMU_DCACHE_CONFIG, 1);
    wr(w, GMU_PWR_COL_INTER_FRAME_CTRL, 0x09c4_0400);
    // RPMH_INTERFACE, LLC, DDR, MX, CX and GFX votes.
    let enables = 1 | (1 << 4) | (1 << 8) | (1 << 9) | (1 << 10) | (1 << 11);
    wr(w, GMU_RPMH_CTRL, rd(w, GMU_RPMH_CTRL) | enables);
}

/// What the boot needs.
pub struct Boot<'a> {
    /// `a630_gmu.bin`.
    pub firmware: &'a [u8],
    pub gx: &'a [hfi::PerfLevel],
    pub cx: &'a [hfi::PerfLevel],
    /// Index into `gx` of the level to start at. Not 0.
    pub initial: usize,
}

/// The bits of Linux's `gmu->status` this path needs (`a6xx_gmu.h`): `GMU_STATUS_FW_START` (the firmware ran far enough that
/// the PDC's sleep sequence may be started) and `GMU_STATUS_PDC_SLEEP` (the sleep sequence ran, so the wake sequence must run
/// before the next start).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Status {
    pub fw_started: bool,
    pub pdc_asleep: bool,
}

/// How the GMU starts: `GMU_COLD_BOOT` writes the firmware, `GMU_WARM_BOOT` runs what its retained memory kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Start {
    Cold,
    Warm,
}

/// `a6xx_gmu_resume`'s choice for the legacy GMU: warm when `GMU_GENERAL_7` (TCM retention, set by every start) reads 1.
pub fn retained<W: RegIo>(w: &W) -> bool {
    rd(w, GMU_GENERAL_7) == 1
}

/// `a6xx_gmu_gx_is_on`: neither the GX head switch nor its clock reports off.
pub fn gx_is_on<W: RegIo>(w: &W) -> bool {
    const GX_HM_GDSC_POWER_OFF: u32 = 1 << 6;
    const GX_HM_CLK_OFF: u32 = 1 << 7;
    rd(w, GMU_SPTPRAC_PWR_CLK_STATUS) & (GX_HM_GDSC_POWER_OFF | GX_HM_CLK_OFF) == 0
}

fn rscc_rd<W: RegIo>(w: &W, reg: u32) -> u32 {
    w.read32(RSCC_OFFSET + reg * 4)
}

/// Whether the GPU's RSC reports its sleep sequence done (`a6xx_rpmh_stop` waits for this bit). Linux keeps that fact in
/// `GMU_STATUS_PDC_SLEEP`; a kernel started after a reload has only the hardware to ask, before [`rpmh_init`] writes the
/// status register again.
pub fn rsc_asleep<W: RegIo>(w: &W) -> bool {
    rscc_rd(w, RSCC_RSC_STATUS0_DRV0) & (1 << 16) != 0
}

/// `a6xx_rpmh_start`: the PDC's wake sequence, only after its sleep sequence ran.
fn rpmh_start<W: RegIo, H: Hal>(w: &W, hal: &H, status: &mut Status) -> Result<()> {
    if !status.pdc_asleep {
        return Ok(());
    }
    status.pdc_asleep = false;
    wr(w, GMU_RSCC_CONTROL_REQ, 1 << 1);
    poll(hal, "GMU: unable to power on the GPU RSC", 10_000, 100, || rd(w, GMU_RSCC_CONTROL_ACK), |v| v & (1 << 1) != 0)?;
    poll(hal, "GMU: GPU RSC sequence stuck while waking up the GPU", 10_000, 100, || rscc_rd(w, RSCC_SEQ_BUSY_DRV0), |v| v == 0)?;
    wr(w, GMU_RSCC_CONTROL_REQ, 0);
    Ok(())
}

/// `a6xx_rpmh_stop` as upstream has it since the PDC sleep fix (Linux 7.1 still tests `GMU_STATUS_FW_START` the wrong way
/// round): the sleep sequence only after the firmware started.
fn rpmh_stop<W: RegIo, H: Hal>(w: &W, hal: &H, status: &mut Status) {
    if !status.fw_started {
        return;
    }
    status.fw_started = false;
    wr(w, GMU_RSCC_CONTROL_REQ, 1);
    if let Err(e) = poll(hal, "GMU: unable to power off the GPU RSC", 10_000, 100, || rscc_rd(w, RSCC_RSC_STATUS0_DRV0), |v| v & (1 << 16) != 0) {
        hal.log(format_args!("gpu: gmu: {}", e));
    }
    wr(w, GMU_RSCC_CONTROL_REQ, 0);
    status.pdc_asleep = true;
}

/// `a6xx_sptprac_enable`, at the idle level `GMU_IDLE_STATE_ACTIVE` where the CPU owns SPTP: Linux reports a failure and goes on.
fn sptprac_enable<W: RegIo, H: Hal>(w: &W, hal: &H) {
    wr(w, GMU_GX_SPTPRAC_POWER_CONTROL, 0x77_8000);
    if let Err(e) = poll(hal, "GMU: unable to power on SPTPRAC", 100, 1, || rd(w, GMU_SPTPRAC_PWR_CLK_STATUS), |v| v & 0x38 == 0x28) {
        hal.log(format_args!("gpu: gmu: {}", e));
    }
}

/// `a6xx_sptprac_disable`: retention on in the GX switch, then SPTP off.
pub fn sptprac_disable<W: RegIo, H: Hal>(w: &W, hal: &H) {
    wr(w, GPU_CC_GX_GDSCR, rd(w, GPU_CC_GX_GDSCR) | (1 << 11));
    wr(w, GMU_GX_SPTPRAC_POWER_CONTROL, 0x77_8001);
    if let Err(e) = poll(hal, "GMU: failed to power off SPTPRAC", 10_000, 100, || rd(w, GMU_SPTPRAC_PWR_CLK_STATUS), |v| v & 0x04 != 0) {
        hal.log(format_args!("gpu: gmu: {}", e));
    }
}

/// `a6xx_gmu_irq_disable`, minus the interrupt lines this driver never wired: mask everything at the GMU.
fn irq_disable<W: RegIo>(w: &W) {
    wr(w, GMU_AO_HOST_INTERRUPT_MASK, u32::MAX);
    wr(w, GMU_GMU2HOST_INTR_MASK, u32::MAX);
}

/// `a6xx_gmu_start`: the core out of reset, and its word that it runs.
fn core_start<W: RegIo, H: Hal>(w: &W, hal: &H, status: &mut Status) -> Result<()> {
    let version = rd(w, GMU_CM3_DTCM_START + 0xff8);
    let (mask, expected) = if version <= 0x2001_0004 { (u32::MAX, 0xbabe_face) } else { (0x1ff, 0x100) };
    hal.log(format_args!("gpu: gmu: starting the core (DTCM version word {:#x})", version));
    wr(w, GMU_CM3_SYSRESET, 1);
    wr(w, GMU_PWR_COL_CP_RESP, 0);
    wr(w, GMU_CM3_SYSRESET, 0);
    hal.barrier();
    // Upstream waits 100 ms here since 7.1 (10 ms before).
    let result = poll(hal, "GMU: firmware initialization timed out", 100_000, 100, || rd(w, GMU_CM3_FW_INIT_RESULT), |v| v & mask == expected);
    status.fw_started = true;
    let result = result?;
    hal.log(format_args!("gpu: gmu: firmware runs, init result {:#x}", result));
    Ok(())
}

/// `a6xx_gmu_resume` from the CX switch on: `a6xx_gmu_fw_start` (cold or warm), `a6xx_hfi_start` and
/// `a6xx_gmu_set_initial_freq`. Each step says what it does first. `queues` is the HFI part of the GMU's memory; the HFI table
/// is written again in it, as an emptied one (`a6xx_hfi_stop`) is the same.
pub fn start<W: RegIo, M: RegIo, H: Hal>(
    w: &W,
    queues: M,
    hal: &H,
    boot: &Boot,
    how: Start,
    status: &mut Status,
) -> Result<hfi::Hfi<M>> {
    if how == Start::Cold
        && (boot.firmware.len() > FIRMWARE_MAX || !boot.firmware.len().is_multiple_of(4) || boot.firmware.is_empty())
    {
        return err("GMU firmware size", boot.firmware.len() as u32);
    }
    if boot.initial == 0 || boot.initial >= boot.gx.len() {
        return err("GMU: bad initial level", boot.initial as u32);
    }
    wr(w, GMU_AO_HOST_INTERRUPT_CLR, u32::MAX);
    wr(w, GMU_AO_HOST_INTERRUPT_MASK, u32::MAX);
    if how == Start::Cold {
        // Linux meets a GMU that is off on a cold start. After a reload this kernel may meet one running: hold it in reset
        // while its memory changes.
        wr(w, GMU_CM3_SYSRESET, 1);
        wr(w, GMU_GMU2HOST_INTR_CLR, u32::MAX);
        wr(w, GMU_GMU2HOST_INTR_MASK, u32::MAX);
    }
    hal.log(format_args!("gpu: gmu: GMU_GENERAL_7 was {:#x}, {:?} start, PDC {}", rd(w, GMU_GENERAL_7), how,
        if status.pdc_asleep { "asleep" } else { "awake" }));
    // "Turn on TCM (Tightly Coupled Memory) retention".
    wr(w, GMU_GENERAL_7, 1);
    rpmh_start(w, hal, status)?;
    if how == Start::Cold {
        hal.log(format_args!("gpu: gmu: writing {} bytes of firmware to its ITCM", boot.firmware.len()));
        for (index, chunk) in boot.firmware.as_chunks::<4>().0.iter().enumerate() {
            wr(w, GMU_CM3_ITCM_START + index as u32, u32::from_le_bytes(*chunk));
        }
        hal.barrier();
        // Read back what the core will run: a word that did not stick says the window or the clock is wrong, before it starts.
        let first = u32::from_le_bytes([boot.firmware[0], boot.firmware[1], boot.firmware[2], boot.firmware[3]]);
        let seen = rd(w, GMU_CM3_ITCM_START);
        if seen != first {
            return err("GMU: ITCM did not take the firmware", seen);
        }
    }

    wr(w, GMU_CM3_FW_INIT_RESULT, 0);
    wr(w, GMU_CM3_BOOT_CONFIG, 2);
    wr(w, GMU_HFI_QTBL_ADDR, HFI_IOVA);
    wr(w, GMU_HFI_QTBL_INFO, 1);
    wr(w, GMU_AHB_FENCE_RANGE_0, (1 << 31) | (0xa << 18) | 0xa0);
    wr(w, GMU_CM3_CFG, 0x4052);
    wr(w, GMU_HFI_SFR_ADDR, gmu_chip_id(CHIP_ID));
    wr(w, GMU_PWR_COL_CP_MSG, LOG_IOVA | (LOG_SIZE / 0x1000 - 1));
    power_config(w);
    core_start(w, hal, status)?;

    // `a6xx_gmu_gfx_rail_on`.
    let vote = boot.gx[boot.gx.len() - 1].vote;
    hal.log(format_args!("gpu: gmu: GX rail: asking for vote {:#x}", vote));
    wr(w, GMU_BOOT_SLUMBER_OPTION, 0);
    wr(w, GMU_GX_VOTE_IDX, vote & 0xff);
    wr(w, GMU_MX_VOTE_IDX, (vote >> 8) & 0xff);
    set_oob(w, hal, OOB_BOOT_SLUMBER, "GMU: no answer to BOOT_SLUMBER, the GX rail")?;

    hal.log(format_args!("gpu: gmu: SPTP power on"));
    sptprac_enable(w, hal);

    hal.log(format_args!("gpu: gmu: HFI queues"));
    let mut hfi = hfi::Hfi::new(queues, HFI_IOVA);
    hal.barrier();
    wr(w, GMU_HFI_CTRL_INIT, 1);
    poll(hal, "GMU: HFI does not start", 10_000, 100, || rd(w, GMU_HFI_CTRL_STATUS), |v| v & 1 != 0)?;
    hal.barrier();
    let boot_state = match how {
        Start::Cold => hfi::GMU_COLD_BOOT,
        Start::Warm => hfi::GMU_WARM_BOOT,
    };
    hfi.start_v1(hal, w, DEBUG_IOVA, DEBUG_SIZE, boot_state, boot.gx, boot.cx)?;
    set_perf(w, hal, boot.initial as u32)?;
    hal.log(format_args!("gpu: gmu: ready at level {}, {} kHz", boot.initial, boot.gx[boot.initial].khz));
    Ok(hfi)
}

/// The first start of this kernel: always cold, since only this kernel knows the firmware it carries. Kept for the
/// bring-up's callers.
pub fn boot<W: RegIo, M: RegIo, H: Hal>(w: &W, queues: M, hal: &H, boot: &Boot, status: &mut Status) -> Result<hfi::Hfi<M>> {
    start(w, queues, hal, boot, Start::Cold, status)
}

/// `a6xx_gmu_isidle`.
pub fn isidle<W: RegIo>(w: &W) -> bool {
    rd(w, GMU_AO_GPU_CX_BUSY_STATUS) & GPU_BUSY_IGN_AHB == 0
}
const GPU_BUSY_IGN_AHB: u32 = 1 << 23;
/// `ADRENO_IDLE_TIMEOUT`, the limit of `spin_until`.
const IDLE_TIMEOUT_US: u64 = 1_000_000;
/// `GMU_IDLE_STATE_ACTIVE`: Linux's A618 has no `ADRENO_QUIRK_IFPC` (`a6xx_catalog.c`, also upstream), so the GMU never
/// collapses GX by itself and slumber is the only power-down.
const IDLE_STATE_ACTIVE: u32 = 0;
/// What `GPU_GMU_CX_GMU_RPMH_POWER_STATE` says after a slumber.
const POWER_STATE_SLUMBER: u32 = 0xf;

/// `a6xx_gmu_wait_for_idle` at the idle level `GMU_IDLE_STATE_ACTIVE`.
pub fn wait_for_idle<W: RegIo, H: Hal>(w: &W, hal: &H) -> Result<()> {
    poll(hal, "GMU: does not reach its idle level", IDLE_TIMEOUT_US, 1, || rd(w, GMU_RPMH_POWER_STATE), |v| v == IDLE_STATE_ACTIVE)
        .map(|_| ())
}

/// `a6xx_gmu_notify_slumber`, legacy: the power counter off, SPTP off (the CPU owns it at this idle level), then the
/// BOOT_SLUMBER handshake with the slumber option, after which the GMU has switched GX off.
fn notify_slumber<W: RegIo, H: Hal>(w: &W, hal: &H) -> Result<()> {
    wr(w, GMU_POWER_COUNTER_ENABLE, 0);
    sptprac_disable(w, hal);
    wr(w, GMU_BOOT_SLUMBER_OPTION, 1);
    let result = set_oob(w, hal, OOB_BOOT_SLUMBER, "GMU: no answer to BOOT_SLUMBER, slumber");
    clear_oob(w, OOB_BOOT_SLUMBER);
    let result = result.and_then(|()| {
        let state = rd(w, GMU_RPMH_POWER_STATE);
        if state != POWER_STATE_SLUMBER { err("GMU: the GMU did not go into slumber", state) } else { Ok(()) }
    });
    // "Put fence into allow mode". No GEMNoC workaround: Linux has it for A621, 7c3 and A7xx only.
    wr(w, GMU_AO_AHB_FENCE_CTRL, 0);
    result
}

/// How a shutdown went.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shutdown {
    /// The GMU slumbered and switched GX off itself.
    Slumbered,
    /// `a6xx_gmu_force_off` had to do it, after this step failed.
    Forced(crate::Error),
}

/// `a6xx_gmu_shutdown`: the GPU idle, its way to memory halted, slumber, the HFI queues emptied, the GMU's interrupts
/// masked, the RSC asleep. A step that fails turns into [`force_off`], as in Linux. `hw_ready` is `!needs_hw_init`: a
/// GMU woken without the GPU's `hw_init` needs the GPU_SET handshake once first, or its power state goes wrong.
pub fn shutdown<W: RegIo, M: RegIo, H: Hal>(
    w: &W,
    hal: &H,
    hfi: Option<&mut hfi::Hfi<M>>,
    hw_ready: bool,
    status: &mut Status,
) -> Shutdown {
    let mut hfi = hfi;
    let graceful = (|| {
        if !hw_ready {
            set_oob(w, hal, OOB_GPU_SET, "GMU: no answer to GPU_SET before slumber")?;
            clear_oob(w, OOB_GPU_SET);
        }
        wait_for_idle(w, hal)?;
        crate::cp::bus_halt(w, hal, false)?;
        notify_slumber(w, hal)?;
        Ok(())
    })();
    if let Err(e) = graceful {
        hal.log(format_args!("gpu: gmu: {}: forcing it off", e));
        force_off(w, hal, hfi.as_deref_mut(), status);
        return Shutdown::Forced(e);
    }
    if let Err(e) = poll(hal, "GMU: unable to slumber", 10_000, 100, || rd(w, GMU_AO_GPU_CX_BUSY_STATUS), |v| v & GPU_BUSY_IGN_AHB == 0) {
        hal.log(format_args!(
            "gpu: gmu: {}: status {:#x}/{:#x}",
            e,
            rd(w, GMU_AO_GPU_CX_BUSY_STATUS),
            rd(w, GMU_AO_GPU_CX_BUSY_STATUS2)
        ));
    }
    if let Some(hfi) = hfi {
        let left = hfi.stop();
        if left != 0 {
            hal.log(format_args!("gpu: hfi: queues {:#x} were not empty", left));
        }
    }
    irq_disable(w);
    rpmh_stop(w, hal, status);
    Shutdown::Slumbered
}

/// `a6xx_gmu_rpmh_off`: no RPMh vote of the GPU's RSC left in flight.
fn rpmh_off<W: RegIo, H: Hal>(w: &W, hal: &H) {
    for (reg, timeout) in [
        (RSCC_TCS0_DRV0_STATUS, 10_000),
        (RSCC_TCS1_DRV0_STATUS, 10_000),
        (RSCC_TCS2_DRV0_STATUS, 10_000),
        (RSCC_TCS3_DRV0_STATUS, 1_000),
    ] {
        let _ = poll(hal, "GMU: RPMh vote in flight", timeout, 100, || rscc_rd(w, reg), |v| v & 1 != 0);
    }
}

/// `a6xx_gmu_force_off`, for a GMU that does not answer. The GPU-side halt and reset touch GX registers; this kernel
/// does them only while GX reports on, since a read behind a switch that is off hangs the bus here.
pub fn force_off<W: RegIo, M: RegIo, H: Hal>(w: &W, hal: &H, hfi: Option<&mut hfi::Hfi<M>>, status: &mut Status) {
    wr(w, GMU_PWR_COL_KEEPALIVE, 0);
    if let Some(hfi) = hfi {
        hfi.stop();
    }
    irq_disable(w);
    sptprac_disable(w, hal);
    rpmh_off(w, hal);
    wr(w, GMU_AHB_FENCE_STATUS_CLR, 0x7);
    wr(w, GMU_AO_AHB_FENCE_CTRL, 0);
    hal.barrier();
    wr(w, GMU_CM3_SYSRESET, 1);
    if gx_is_on(w) {
        if let Err(e) = crate::cp::bus_halt(w, hal, true) {
            hal.log(format_args!("gpu: gmu: force off: {}", e));
        }
        crate::cp::sw_reset(w, hal, true);
    } else {
        hal.log(format_args!("gpu: gmu: force off: GX is off, no GPU halt or reset"));
    }
    rpmh_stop(w, hal, status);
}

/// `a6xx_gmu_set_freq`, legacy: a new level from the performance table.
pub fn set_perf<W: RegIo, H: Hal>(w: &W, hal: &H, index: u32) -> Result<()> {
    wr(w, GMU_DCVS_ACK_OPTION, 0);
    wr(w, GMU_DCVS_PERF_SETTING, (3 << 28) | index);
    // "Send an invalid index as a vote for the bus bandwidth and let the bus scaling code figure it out".
    wr(w, GMU_DCVS_BW_SETTING, 0xff);
    set_oob(w, hal, OOB_DCVS, "GMU: no answer to a frequency change")?;
    clear_oob(w, OOB_DCVS);
    let answer = rd(w, GMU_DCVS_RETURN);
    if answer != 0 {
        return err("GMU: frequency change refused", answer);
    }
    Ok(())
}

/// What a stuck GMU shows.
pub fn report<W: RegIo, H: Hal>(w: &W, hal: &H) {
    hal.log(format_args!(
        "gpu: gmu: init {:#x} hfi {:#x} intr {:#x} ao {:#x} sptprac {:#x} rpmh {:#x}",
        rd(w, GMU_CM3_FW_INIT_RESULT),
        rd(w, GMU_HFI_CTRL_STATUS),
        rd(w, GMU_GMU2HOST_INTR_INFO),
        rd(w, GMU_AO_HOST_INTERRUPT_STATUS),
        rd(w, GMU_SPTPRAC_PWR_CLK_STATUS),
        rd(w, GMU_RPMH_CTRL),
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_memory_layout_matches_linuxs_allocation_order() {
        assert_eq!(DEBUG_IOVA, 0x6000_1000);
        assert_eq!(LOG_IOVA, 0x6000_5000);
        assert_eq!(HFI_IOVA, 0x6000_9000);
        assert_eq!(MEMORY_SIZE, 0xd000);
    }

    #[test]
    fn the_gmu_packs_the_chip_id_its_own_way() {
        assert_eq!(gmu_chip_id(CHIP_ID), 0x0601_8000);
    }
}
