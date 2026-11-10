// SPDX-License-Identifier: GPL-2.0-only
//! The block's processor between sessions: Linux's runtime suspend and resume for HFI 3xx/4xx (`venus_suspend_3xx` with
//! `venus_prepare_power_collapse` and `venus_power_off`, then `venus_resume` with `venus_power_on`, all in hfi_venus.c). The clocks,
//! the switch and the votes around them are the kernel's (`core_power_v4`, `venus_runtime_suspend`); `docs/research/juno-venus.md`
//! section 3.6 has the order.

use lucina_qcom_soc::io::RegIo;

use crate::{
    Hal, Result,
    cpu::Core,
    err, hfi,
    iface::Shared,
    regs,
};

/// `readx_poll_timeout(..., 1500, 100 * 1500)` in `venus_suspend_3xx`.
pub const IDLE_POLL_US: u32 = 1500;
pub const IDLE_TIMEOUT_US: u64 = 100 * 1500;

/// `pm_runtime_set_autosuspend_delay(dev, 2000)` in vdec.c: how long the block stays up after its last session.
pub const AUTOSUSPEND_MS: u64 = 2000;

/// What the secure world does for the block: `qcom_scm_set_remote_state(resume, 0)` (`venus_set_hw_state` with `use_tz`).
pub trait RemoteState {
    fn set_remote_state(&mut self, resume: bool) -> Result<()>;
}

impl<R: RegIo, S: Shared> Core<R, S> {
    /// `venus_cpu_and_video_core_idle`: the processor in WFI and the firmware's idle message bit set.
    fn idle(&self) -> bool {
        self.regs.read32(regs::WRAPPER_CPU_STATUS) & regs::WRAPPER_CPU_STATUS_WFI != 0
            && self.regs.read32(regs::CPU_CS_SCIACMDARG0) & regs::SCIACMDARG0_INIT_IDLE_MSG != 0
    }

    /// `venus_cpu_idle_and_pc_ready`: in WFI and ready for its power to go.
    fn pc_ready(&self) -> bool {
        self.regs.read32(regs::WRAPPER_CPU_STATUS) & regs::WRAPPER_CPU_STATUS_WFI != 0
            && self.regs.read32(regs::CPU_CS_SCIACMDARG0) & regs::SCIACMDARG0_PC_READY != 0
    }

    /// `venus_suspend_3xx`: if the firmware is not already ready for power collapse, wait until it is idle, send `SYS_PC_PREP`
    /// without waiting for its answer, and wait until it says ready; then `venus_power_off`: the secure world told to suspend it, and
    /// its bus master halted. The caller then turns off its clocks and switch (`core_power_v4(POWER_OFF)`) and drops the bandwidth.
    pub fn suspend<H: Hal, T: RemoteState>(&self, hal: &H, tz: &mut T) -> Result<()> {
        if self.is_dead() {
            return err("suspend: the firmware is in an error state", 0);
        }
        if self.regs.read32(regs::CPU_CS_SCIACMDARG0) & regs::SCIACMDARG0_PC_READY == 0 {
            crate::poll(hal, "suspend: the processor did not go idle", IDLE_TIMEOUT_US, IDLE_POLL_US, || self.idle() as u32, |v| v != 0)
                .map_err(|e| crate::Error { value: self.regs.read32(regs::CPU_CS_SCIACMDARG0), ..e })?;
            self.send(&hfi::sys_pc_prep(), false)?;
            crate::poll(hal, "suspend: the processor did not get ready for power collapse", IDLE_TIMEOUT_US, IDLE_POLL_US, || {
                self.pc_ready() as u32
            }, |v| v != 0)
            .map_err(|e| crate::Error { value: self.regs.read32(regs::CPU_CS_SCIACMDARG0), ..e })?;
        }
        tz.set_remote_state(false)?;
        self.halt_axi(hal)?;
        Ok(())
    }

    /// `venus_resume` → `venus_power_on`, after the caller's votes and `core_power_v4(POWER_ON)`: the secure world told to resume it,
    /// then `venus_run` (the queues where they were, the processor started again). The firmware keeps its state over a power
    /// collapse: there is no second `SYS_INIT`. Returns what [`Core::run`] returns.
    pub fn resume<H: Hal, T: RemoteState>(&self, hal: &H, tz: &mut T) -> Result<(u32, u32)> {
        tz.set_remote_state(true)?;
        match self.run(hal) {
            Ok(v) => Ok(v),
            Err(e) => {
                // venus_power_on: err_suspend.
                let _ = tz.set_remote_state(false);
                Err(e)
            }
        }
    }
}
