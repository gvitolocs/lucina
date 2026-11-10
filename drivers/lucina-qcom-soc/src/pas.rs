//! The order of a modem cold start through the secure world, Linux 7.1 `drivers/remoteproc/qcom_q6v5_pas.c`
//! (`qcom_pas_start`, `mpss_resource_init`) and `qcom_q6v5.c` (GPL-2.0-only). No code from elsewhere.
//!
//! The device tree says `qcom,sc7180-mpss-pas`, so `qcom_q6v5_mss.c` (MBA, GCC resets) does not apply on this laptop.
//! [`Hooks`] are the hardware; this only fixes the order and the unwinding, so a host test can assert both. The kernel
//! runs it for the Wi-Fi modem (`device/qcom_wifi/modem.rs`), proven on the laptop since 2026-11-01.

/// `mpss_resource_init`, the data Linux binds to `qcom,sc7180-mpss-pas`.
pub const MPSS_PAS_ID: u32 = 4;
pub const MPSS_CRASH_REASON_SMEM: u32 = 421;
pub const MPSS_LOAD_STATE: &str = "modem";
pub const MPSS_SYSMON_NAME: &str = "modem";
/// `qcom_q6v5_wait_for_start(.., 5000 ms)`.
pub const START_TIMEOUT_MS: u32 = 5000;

/// The carve-out on this laptop: `mpss@86000000` in the device tree.
pub const MPSS_MEM_PHYS: u64 = 0x8600_0000;
pub const MPSS_MEM_SIZE: u64 = 0x200_0000;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Wait {
    Ready,
    TimedOut,
    Fatal,
}

/// The steps. Each returns `Err` to stop; `E` is the caller's error.
pub trait Hooks {
    type Error;
    /// `q6v5_load_state_toggle`: QMP `load_state` for [`MPSS_LOAD_STATE`].
    fn load_state(&mut self, on: bool) -> Result<(), Self::Error>;
    /// `qcom_pas_pds_enable`: proxy votes for `cx` and `mss` at their highest level.
    fn proxy_votes(&mut self, on: bool) -> Result<(), Self::Error>;
    /// `clk_prepare_enable(xo)`: the RPMh CXO vote.
    fn xo(&mut self, on: bool) -> Result<(), Self::Error>;
    /// `__qcom_mdt_pas_init`: `PAS_INIT_IMAGE`, and `PAS_MEM_SETUP` for a relocatable image.
    fn init_image(&mut self) -> Result<(), Self::Error>;
    /// `qcom_mdt_load_no_init`: segments into the carve-out.
    fn load_segments(&mut self) -> Result<(), Self::Error>;
    /// `qcom_pil_info_store`, the crash-dump aid in IMEM. A failure there does not stop Linux.
    fn pil_info(&mut self);
    /// `PAS_AUTH_AND_RESET`.
    fn auth_and_reset(&mut self) -> Result<(), Self::Error>;
    /// Waits for SMP2P `ready` for [`START_TIMEOUT_MS`], or the fatal bit or watchdog.
    fn wait_ready(&mut self) -> Wait;
    /// `PAS_SHUTDOWN`.
    fn shutdown(&mut self);
    /// `qcom_scm_pas_metadata_release`.
    fn release_metadata(&mut self);
}

#[derive(Debug, PartialEq, Eq)]
pub enum StartError<E> {
    Step(E),
    TimedOut,
    Fatal,
}

/// `qcom_pas_start`. The proxy votes and XO stay on after success: Linux drops them on the `handover` interrupt
/// (`qcom_pas_handover`), and the load state stays on until the modem stops.
pub fn start<H: Hooks>(h: &mut H) -> Result<(), StartError<H::Error>> {
    h.load_state(true).map_err(StartError::Step)?;
    if let Err(e) = h.proxy_votes(true) {
        let _ = h.load_state(false);
        return Err(StartError::Step(e));
    }
    if let Err(e) = h.xo(true) {
        let _ = h.proxy_votes(false);
        let _ = h.load_state(false);
        return Err(StartError::Step(e));
    }
    let result = boot(h);
    if result.is_err() {
        let _ = h.xo(false);
        let _ = h.proxy_votes(false);
        let _ = h.load_state(false);
    }
    result
}

fn boot<H: Hooks>(h: &mut H) -> Result<(), StartError<H::Error>> {
    let loaded = h.init_image().and_then(|()| h.load_segments());
    if let Err(e) = loaded {
        h.release_metadata();
        return Err(StartError::Step(e));
    }
    h.pil_info();
    if let Err(e) = h.auth_and_reset() {
        h.release_metadata();
        return Err(StartError::Step(e));
    }
    match h.wait_ready() {
        Wait::Ready => {
            h.release_metadata();
            Ok(())
        }
        Wait::TimedOut => {
            // Linux shuts down on -ETIMEDOUT only.
            h.shutdown();
            h.release_metadata();
            Err(StartError::TimedOut)
        }
        Wait::Fatal => {
            h.release_metadata();
            Err(StartError::Fatal)
        }
    }
}

/// `qcom_pas_handover`: after the `handover` bit the modem holds its own votes.
pub fn handover<H: Hooks>(h: &mut H) {
    let _ = h.xo(false);
    let _ = h.proxy_votes(false);
}
