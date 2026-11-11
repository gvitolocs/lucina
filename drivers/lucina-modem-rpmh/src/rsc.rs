//! The RSC (Resource State Coordinator) DRV driver: active-only requests through a TCS, polled instead of interrupt
//! driven, register layout 2.7.
//!
//! Derived from Linux 7.1 `drivers/soc/qcom/rpmh-rsc.c`, `rpmh-internal.h` and `include/soc/qcom/tcs.h` (GPL-2.0;
//! Copyright The Linux Foundation, Qualcomm Innovation Center) and, for the shape of a polled transfer, from
//! petitstrawberry/scarlet-project-chromebook `drivers/soc/qcom-rpmh-rsc/src/lib.rs` (GPL-2.0-only). Where the two
//! disagree this file follows Linux; the differences are listed at the end of this comment.
//!
//! What follows which Linux function:
//!
//! * [`Rsc::probe`]: the version check and TCS geometry of `rpmh_rsc_probe` and `rpmh_probe_tcs_config`, plus the
//!   `IRQ_ENABLE` write at the end of probe. Linux picks the 3.0 register table for a major version of 3 or more and the
//!   2.7 table for everything else; this crate only has the 2.7 table, so a major version of 3 or more is an error.
//! * [`Rsc::send`]: `rpmh_rsc_send_data` (`get_tcs_for_msg`, `claim_tcs_for_req`, `check_for_req_inflight`,
//!   `find_free_tcs`, `__tcs_buffer_write`, `__tcs_set_trigger`) followed by what `tcs_tx_done` does when the interrupt
//!   fires. There is no interrupt: `send` polls `RSC_DRV_IRQ_STATUS` for the TCS, then runs the handler's body.
//! * [`Rsc::reap`]: `tcs_tx_done` for a request whose caller gave up waiting.
//!
//! # Active-only
//!
//! [`Rsc::send`] takes only `RPMH_ACTIVE_ONLY_STATE` requests. Sleep and wake sets are [`Rsc::write_ctrl`] and
//! [`crate::rpmh::Cache`]; the CPU PM and genpd notifiers and `rpmh_rsc_write_next_wakeup` are left out: nothing in this
//! kernel powers the cluster down.
//! A DRV with no ACTIVE group borrows its WAKE group, as `get_tcs_for_msg` does, and clears the stale wake commands and
//! switches the TCS interrupt bit on and off around the use, as `rpmh_rsc_send_data` and `tcs_tx_done` do.
//!
//! # `wait`
//!
//! `rpmh_write` sets `wait_for_compl`; `rpmh_write_async` does not. Both put the same single command in a TCS and
//! trigger it. The only difference on the wire is the response-required bit in `RSC_DRV_CMD_MSGID`
//! ([`CMD_MSGID_RESP_REQ`]), which makes the accelerator acknowledge the command before the TCS reports done. Linux
//! returns from an async write right after the trigger and lets the interrupt handler reclaim the TCS. There is no
//! handler here, so [`Rsc::send`] always waits for the TCS to report done, async or not; the flag only sets the bit.
//!
//! # Same-address conflicts
//!
//! `check_for_req_inflight` exists because the hardware does not like a second request to an address while one is
//! in flight. With one request at a time that cannot happen: `send` returns only after the TCS is reclaimed. It
//! can happen after a timeout, because the TCS then stays claimed (as in Linux, whose `rpmh_write` times out after
//! 10 s without touching the TCS). The check is ported for that case and reads the in-use TCSes' registers just as
//! Linux does; [`Rsc::reap`] frees a TCS whose completion arrived late.
//!
//! # Differences from Linux (all deliberate)
//!
//! * Linux waits forever for a free TCS; this returns [`RscError::Busy`].
//! * Linux does not check the command count against `ncpt`, nor an address of 0; this does.
//! * Linux 7.1 does not write `RSC_DRV_CMD_WAIT_FOR_CMPL` at all (the response-required bit in MSGID replaced it).
//!   Scarlet writes it. [`Quirks::write_wait_for_cmpl`] writes it as Scarlet does (after `CMD_ENABLE`, before the
//!   trigger, cleared on completion), off by default.
//! * Linux 7.1 does not read `RSC_DRV_CMD_STATUS` on completion. Kernels before 5.7 did and failed the request when a
//!   command lacked `CMD_STATUS_ISSUED`, or lacked `CMD_STATUS_COMPL` when a response was required.
//!   [`Quirks::check_cmd_status`] (on by default) does that check, after the TCS is reclaimed, and reports it as
//!   [`RscError::CmdNotAcked`]. The status registers are sticky across TCS reuse, so a stale bit can only hide a failure.
//! * Linux's `write_tcs_reg_sync` logs an error and carries on when the read-back never matches; this returns
//!   [`RscError::RegisterStuck`]. The wait is a poll count (it has no clock), not Linux's 1 second of `udelay(1)`.
//! * `probe` also checks `ncpt` (1 to 16) and that the whole register window the geometry implies fits the mapping.

use core::fmt;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use lucina_qcom_soc::io::{Delay, RegIo};

/// What Linux keeps once per DRV in `struct rsc_drv`: `drv->lock` and `drv->tcs_in_use`. The kernel used to build an [`Rsc`]
/// in each driver that votes (Wi-Fi's modem, audio, Venus, the SD rails), all on the same DRV; without one lock and one
/// in-use mask they took TCS 0 at the same time. Every [`Rsc`] whose [`RscConfig::bus`] names this shares both. The kernel
/// now has one [`Rsc`] for the DRV, in a [`Drv`], whose lock is the one its transactions wait on; this stays as the guard
/// should a second handle ever be made.
///
/// Linux holds the lock while it claims a TCS and writes it, and releases it before the wait, so two requests can be in
/// flight on the two ACTIVE TCSes. Here the lock is held through the wait too (no interrupt to finish a request from),
/// so requests go one after the other. The in-use mask still matters: a TCS that timed out stays claimed for every
/// driver, as in Linux, and the next request takes the other one.
pub struct Bus {
    busy: AtomicBool,
    in_use: AtomicU32,
}

impl Bus {
    pub const fn new() -> Self {
        Bus {
            busy: AtomicBool::new(false),
            in_use: AtomicU32::new(0),
        }
    }

    /// Whether some [`Rsc`] holds the lock now.
    pub fn is_busy(&self) -> bool {
        self.busy.load(Ordering::Acquire)
    }

    /// The TCSes claimed by any [`Rsc`] on this DRV.
    pub fn in_use(&self) -> u32 {
        self.in_use.load(Ordering::Acquire)
    }
}

impl Default for Bus {
    fn default() -> Self {
        Self::new()
    }
}

/// Two configurations are equal when they name the same bus, not two that happen to hold the same mask.
impl PartialEq for Bus {
    fn eq(&self, other: &Self) -> bool {
        core::ptr::eq(self, other)
    }
}

impl Eq for Bus {}

impl fmt::Debug for Bus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Bus")
            .field("busy", &self.is_busy())
            .field("in_use", &self.in_use())
            .finish()
    }
}

/// The lock of [`Bus`] while held; released on drop.
struct Held<'a>(Option<&'a Bus>);

impl<'a> Held<'a> {
    /// Waits for the lock: 1 ms sleeps of `delay` when there is one, a spin otherwise.
    fn take(bus: Option<&'a Bus>, delay: Option<&dyn Delay>) -> Self {
        if let Some(bus) = bus {
            while bus
                .busy
                .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_err()
            {
                match delay {
                    Some(delay) => delay.sleep_ms(1),
                    None => core::hint::spin_loop(),
                }
            }
        }
        Held(bus)
    }
}

impl Drop for Held<'_> {
    fn drop(&mut self) {
        if let Some(bus) = self.0 {
            bus.busy.store(false, Ordering::Release);
        }
    }
}

/// `RPMH_TIMEOUT_MS` of `rpmh.c`: how long `rpmh_write` waits for a request's answer, the `timeout_ms` to pass to
/// [`Rsc::send`] for a waiting request.
pub const RPMH_TIMEOUT_MS: u32 = 10_000;

/// The [`Bus`] of the Galaxy Book Go's `drv-2`, the one DRV every voting driver of this kernel uses.
pub static GALAXY_BOOK_GO_DRV2_BUS: Bus = Bus::new();

/// A DRV's register window behind a box, so that drivers whose own register types differ can share one [`Drv`].
pub struct Window(pub alloc::boxed::Box<dyn RegIo + Send>);

impl RegIo for Window {
    fn read32(&self, off: u32) -> u32 {
        self.0.read32(off)
    }

    fn write32(&self, off: u32, value: u32) {
        self.0.write32(off, value)
    }
}

/// Linux's one `struct rsc_drv` per DRV, for a whole kernel: the one [`Rsc`] every driver voting on the DRV uses, probed once
/// (`rpmh_rsc_probe`), and the lock each transaction takes (`drv->lock` of `rpmh_rsc_send_data`).
///
/// [`Drv::with`] holds the lock for what its closure does, and callers give it one transaction: a request's TCS write, trigger
/// and wait for completion ([`Rsc::send`]), one `rpmh_flush` ([`crate::rpmh::Cache::flush`]) or one read of the slots
/// ([`Rsc::slots`]). Linux lets go of `drv->lock` before the wait, which its interrupt ends; here nothing but the poll ends a
/// request, so the lock is held through the wait, as [`Bus`] already was. Waiting for the lock sleeps in 1 ms steps of the
/// caller's [`Delay`]: it may sleep, so never from interrupt context or the idle path.
pub struct Drv {
    rsc: spin::Mutex<Option<Rsc<Window>>>,
}

impl Drv {
    pub const fn new() -> Self {
        Drv {
            rsc: spin::Mutex::new(None),
        }
    }

    fn lock(&self, delay: &impl Delay) -> spin::MutexGuard<'_, Option<Rsc<Window>>> {
        loop {
            if let Some(held) = self.rsc.try_lock() {
                return held;
            }
            delay.sleep_ms(1);
        }
    }

    /// The probe, once: the first caller's `probe` makes the [`Rsc`], under the lock; a later call finds it and probes nothing.
    pub fn open<E>(
        &self,
        delay: &impl Delay,
        probe: impl FnOnce() -> Result<Rsc<Window>, E>,
    ) -> Result<(), E> {
        let mut held = self.lock(delay);
        if held.is_none() {
            *held = Some(probe()?);
        }
        Ok(())
    }

    /// `f` on the one [`Rsc`] with the lock held, then the lock let go; `None` before [`Drv::open`]. Give it one transaction.
    pub fn with<T>(&self, delay: &impl Delay, f: impl FnOnce(&mut Rsc<Window>) -> T) -> Option<T> {
        self.lock(delay).as_mut().map(f)
    }
}

impl Default for Drv {
    fn default() -> Self {
        Self::new()
    }
}

/// The one [`Rsc`] of the Galaxy Book Go's `drv-2`, for every driver of the kernel (Wi-Fi's modem, the vote table, Venus, the SD
/// rails).
pub static GALAXY_BOOK_GO_DRV2_RSC: Drv = Drv::new();

/// Register offsets of layout version 2.7 (`rpmh_rsc_reg_offset_ver_2_7`) and the fixed `RSC_DRV_ID`.
///
/// The IRQ registers are relative to the TCS base (`drv->tcs_base`, the DRV base plus `qcom,tcs-offset`), as are the
/// per-TCS registers once the TCS number is applied (`tcs_reg_addr`); `RSC_DRV_ID`, `DRV_SOLVER_CONFIG` and
/// `DRV_PRNT_CHLD_CONFIG` are relative to the DRV base.
pub mod reg {
    /// `#define RSC_DRV_ID 0`.
    pub const RSC_DRV_ID: u32 = 0x00;
    /// `RSC_DRV_TCS_OFFSET`: distance between two TCS register blocks.
    pub const TCS_OFFSET: u32 = 672;
    /// `RSC_DRV_CMD_OFFSET`: distance between two commands in a TCS.
    pub const CMD_OFFSET: u32 = 20;
    pub const DRV_SOLVER_CONFIG: u32 = 0x04;
    pub const DRV_PRNT_CHLD_CONFIG: u32 = 0x0c;
    pub const IRQ_ENABLE: u32 = 0x00;
    pub const IRQ_STATUS: u32 = 0x04;
    pub const IRQ_CLEAR: u32 = 0x08;
    pub const CMD_WAIT_FOR_CMPL: u32 = 0x10;
    pub const CONTROL: u32 = 0x14;
    pub const STATUS: u32 = 0x18;
    pub const CMD_ENABLE: u32 = 0x1c;
    pub const CMD_MSGID: u32 = 0x30;
    pub const CMD_ADDR: u32 = 0x34;
    pub const CMD_DATA: u32 = 0x38;
    pub const CMD_STATUS: u32 = 0x3c;
    pub const CMD_RESP_DATA: u32 = 0x40;
}

/// `DRV_NUM_TCS_MASK`, `DRV_NUM_TCS_SHIFT`: the TCS count of DRV `n` sits at bit `6 * n` of `DRV_PRNT_CHLD_CONFIG`.
pub const DRV_NUM_TCS_MASK: u32 = 0x3f;
pub const DRV_NUM_TCS_SHIFT: u32 = 6;
/// `DRV_NCPT_MASK`, `DRV_NCPT_SHIFT`: commands per TCS.
pub const DRV_NCPT_MASK: u32 = 0x1f;
pub const DRV_NCPT_SHIFT: u32 = 27;

/// `TCS_AMC_MODE_ENABLE`, `TCS_AMC_MODE_TRIGGER` in the TCS `CONTROL` register.
pub const TCS_AMC_MODE_ENABLE: u32 = 1 << 16;
pub const TCS_AMC_MODE_TRIGGER: u32 = 1 << 24;
/// `CMD_MSGID_LEN`, `CMD_MSGID_RESP_REQ`, `CMD_MSGID_WRITE`.
pub const CMD_MSGID_LEN: u32 = 8;
pub const CMD_MSGID_RESP_REQ: u32 = 1 << 8;
pub const CMD_MSGID_WRITE: u32 = 1 << 16;
/// `CMD_STATUS_ISSUED`, `CMD_STATUS_COMPL`.
pub const CMD_STATUS_ISSUED: u32 = 1 << 8;
pub const CMD_STATUS_COMPL: u32 = 1 << 16;

/// `MAX_CMDS_PER_TCS`, `MAX_TCS_PER_TYPE`, `TCS_TYPE_NR`, `MAX_TCS_NR`.
pub const MAX_CMDS_PER_TCS: usize = 16;
pub const MAX_TCS_PER_TYPE: u32 = 3;
pub const TCS_TYPE_NR: usize = 4;
pub const MAX_TCS_NR: usize = MAX_TCS_PER_TYPE as usize * TCS_TYPE_NR;

/// TCS group types (`dt-bindings/soc/qcom,rpmh-rsc.h`), also the index into [`Rsc::group`].
pub const SLEEP_TCS: usize = 0;
pub const WAKE_TCS: usize = 1;
pub const ACTIVE_TCS: usize = 2;
pub const CONTROL_TCS: usize = 3;

/// How many times a register is read back before `write_tcs_reg_sync` gives up. Linux allows `USEC_PER_SEC` reads
/// with a microsecond between them; with no clock here the count alone bounds it.
pub const SYNC_READBACK_POLLS: u32 = 100_000;
/// Reads of `IRQ_STATUS` made back to back before [`Rsc::send`] starts sleeping between them. RPMh normally answers
/// in microseconds, so most requests never reach the sleeping loop.
pub const SPIN_POLLS: u32 = 2000;

/// `struct tcs_cmd`. `wait` is a bool here (`u32` in Linux): make the accelerator acknowledge this command before the
/// TCS reports done. In a one-command request it has the same effect as the request's `wait_for_compl`, and the vote
/// lists of this crate use it that way (see [`TcsRequest`]).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct TcsCmd {
    pub addr: u32,
    pub data: u32,
    pub wait: bool,
}

/// `struct tcs_request` for the active state: `wait_for_compl` marks every command response-required, as
/// `rpmh_write` does; `cmds` go into one TCS.
#[derive(Copy, Clone, Debug)]
pub struct TcsRequest<'a> {
    pub wait_for_compl: bool,
    pub cmds: &'a [TcsCmd],
}

impl TcsCmd {
    /// The one-command request `rpmh_write` (`wait`) or `rpmh_write_async` (no `wait`) would make. The command's own
    /// `wait` moves into the request, as Linux's callers never set `cmd->wait`.
    pub fn as_request<'a>(&'a self, one: &'a mut [TcsCmd; 1]) -> TcsRequest<'a> {
        one[0] = TcsCmd {
            wait: false,
            ..*self
        };
        TcsRequest {
            wait_for_compl: self.wait,
            cmds: &one[..],
        }
    }
}

/// The values from the device tree node of the RSC that `rpmh_rsc_probe` reads.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct RscConfig {
    /// `qcom,drv-id`: which DRV window this is (the register window passed to [`Rsc::probe`] is that DRV's `reg`).
    pub drv_id: u32,
    /// `qcom,tcs-offset`: from the DRV base to the IRQ registers and TCS 0.
    pub tcs_offset: u32,
    /// `qcom,tcs-config`: four (type, count) pairs, types as [`ACTIVE_TCS`] and the others.
    pub tcs_config: [(u32, u32); TCS_TYPE_NR],
    /// Size in bytes of the mapped DRV window (`reg` size). Nothing past it is touched.
    pub window: u32,
    /// The lock and in-use mask shared with every other [`Rsc`] on this DRV. `None` gives a driver of its own, for
    /// tests and models only: the kernel must not build two on one DRV without it.
    pub bus: Option<&'static Bus>,
}

impl RscConfig {
    /// Samsung Galaxy Book Go, SC7180: `drv-2` at 0x18220000 (size 0x10000), `qcom,tcs-offset = <0xd00>`,
    /// `qcom,tcs-config = <2 2 0 3 1 3 3 1>`: two ACTIVE, three SLEEP, three WAKE and one CONTROL TCS.
    pub const GALAXY_BOOK_GO_DRV2: RscConfig = RscConfig {
        drv_id: 2,
        tcs_offset: 0xd00,
        tcs_config: [(2, 2), (0, 3), (1, 3), (3, 1)],
        window: 0x10000,
        bus: Some(&GALAXY_BOOK_GO_DRV2_BUS),
    };
}

/// `struct tcs_group` as `rpmh_probe_tcs_config` fills it.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct TcsGroup {
    /// The group appeared in `qcom,tcs-config` (Linux's `tcs->drv` being set).
    pub configured: bool,
    pub num_tcs: u32,
    pub ncpt: u32,
    /// Bit mask of the group's TCSes among all TCSes of the DRV.
    pub mask: u32,
    /// Number of the group's first TCS.
    pub offset: u32,
}

/// Switches for the places where this crate goes beyond or away from Linux 7.1; see the module comment.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Quirks {
    /// Write `RSC_DRV_CMD_WAIT_FOR_CMPL` with the mask of response-required commands before the trigger (Scarlet
    /// does; Linux 7.1 never writes the register). Default off.
    pub write_wait_for_cmpl: bool,
    /// Read `RSC_DRV_CMD_STATUS` on completion and fail a command that was not issued or, when a response was
    /// required, not completed. Default on.
    pub check_cmd_status: bool,
}

impl Default for Quirks {
    fn default() -> Self {
        Quirks {
            write_wait_for_cmpl: false,
            check_cmd_status: true,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum RscError {
    /// `RSC_DRV_ID` major version 3 or more: that is Linux's 3.0 register table, which this crate does not have.
    UnsupportedVersion {
        major: u32,
        minor: u32,
    },
    /// The device tree values or the hardware's `DRV_PRNT_CHLD_CONFIG` do not describe a usable DRV (`-EINVAL`).
    BadConfig(&'static str),
    /// The DRV has neither an ACTIVE nor a WAKE TCS.
    NoActiveTcs,
    /// Every TCS of the group is in use, or one in use has a command to the same resource (`-EBUSY`).
    Busy,
    EmptyRequest,
    /// More commands than the TCS holds (`ncpt`, at most 16).
    TooManyCmds {
        max: usize,
    },
    /// Command number `index` has address 0, which no resource has.
    BadAddress {
        index: usize,
    },
    /// A register did not read back what was written to it.
    RegisterStuck {
        tcs: u32,
        reg: u32,
    },
    /// The TCS did not report done in time. It stays claimed: [`Rsc::reap`] frees it once it does.
    Timeout {
        tcs: u32,
    },
    /// The TCS is reclaimed but command `cmd` has status `status` (see [`Quirks::check_cmd_status`]).
    CmdNotAcked {
        tcs: u32,
        cmd: u32,
        status: u32,
    },
}

impl fmt::Display for RscError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            RscError::UnsupportedVersion { major, minor } => {
                write!(
                    f,
                    "RSC version {major}.{minor}: only layout 2.7 (major < 3) is supported"
                )
            }
            RscError::BadConfig(why) => write!(f, "RSC configuration: {why}"),
            RscError::NoActiveTcs => f.write_str("RSC has no ACTIVE or WAKE TCS"),
            RscError::Busy => {
                f.write_str("no free TCS, or a request to the same resource is in flight")
            }
            RscError::EmptyRequest => f.write_str("request has no commands"),
            RscError::TooManyCmds { max } => write!(f, "more than {max} commands in one request"),
            RscError::BadAddress { index } => write!(f, "command {index} has address 0"),
            RscError::RegisterStuck { tcs, reg } => {
                write!(
                    f,
                    "TCS {tcs} register {reg:#x} did not read back what was written"
                )
            }
            RscError::Timeout { tcs } => write!(f, "TCS {tcs} did not complete in time"),
            RscError::CmdNotAcked { tcs, cmd, status } => {
                write!(
                    f,
                    "TCS {tcs} command {cmd} finished with status {status:#x}"
                )
            }
        }
    }
}

/// `cmd_db_match_resource_addr`: the same address, or two registers of the same VRM resource (VRM resources have
/// three or four contiguous word registers; the bits below bit 4 pick one).
pub fn match_resource_addr(a: u32, b: u32) -> bool {
    const SLAVE_ID_VRM: u32 = 4;
    let slave = |addr: u32| (addr >> 16) & 0xf;
    let vrm_addr = |addr: u32| (addr >> 4) & 0xffff;
    a == b || (slave(a) == SLAVE_ID_VRM && vrm_addr(a) == vrm_addr(b))
}

/// One probed DRV.
pub struct Rsc<R: RegIo> {
    io: R,
    version: (u32, u32),
    tcs_base: u32,
    groups: [TcsGroup; TCS_TYPE_NR],
    num_tcs: u32,
    /// This driver's claimed TCSes when it has no [`Bus`]; with one, the bus's mask is the truth.
    in_use: u32,
    bus: Option<&'static Bus>,
    /// `tcs_group::slots` of the SLEEP and WAKE groups: the commands written and not yet invalidated.
    ctrl_slots: [u64; 2],
    pub quirks: Quirks,
}

impl<R: RegIo> Rsc<R> {
    /// `rpmh_rsc_probe` as far as it concerns the register file: read the version, read the TCS geometry
    /// (`rpmh_probe_tcs_config`), then write the ACTIVE group's mask to `IRQ_ENABLE` ("enable the active TCS to send
    /// requests immediately"). `io` is the DRV's `reg` window, offset 0 being `RSC_DRV_ID`.
    ///
    /// Writing `IRQ_ENABLE` arms the interrupt line of the DRV; nothing here handles it. Whether `IRQ_STATUS` latches
    /// without it is not known, so the write stays as Linux has it.
    pub fn probe(io: R, cfg: &RscConfig) -> Result<Self, RscError> {
        let id = io.read32(reg::RSC_DRV_ID);
        let major = (id >> 16) & 0xff;
        let minor = (id >> 8) & 0xff;
        if major >= 3 {
            return Err(RscError::UnsupportedVersion { major, minor });
        }

        // rpmh_probe_tcs_config
        let config = io.read32(reg::DRV_PRNT_CHLD_CONFIG);
        let shift = DRV_NUM_TCS_SHIFT.saturating_mul(cfg.drv_id);
        if shift >= 32 {
            return Err(RscError::BadConfig(
                "qcom,drv-id does not fit DRV_PRNT_CHLD_CONFIG",
            ));
        }
        let max_tcs = (config >> shift) & DRV_NUM_TCS_MASK;
        let ncpt = (config >> DRV_NCPT_SHIFT) & DRV_NCPT_MASK;
        if ncpt == 0 || ncpt as usize > MAX_CMDS_PER_TCS {
            return Err(RscError::BadConfig("commands per TCS is 0 or more than 16"));
        }

        for &(ty, n) in &cfg.tcs_config {
            if ty as usize >= TCS_TYPE_NR {
                return Err(RscError::BadConfig("TCS type out of range"));
            }
            if n > MAX_TCS_PER_TYPE {
                return Err(RscError::BadConfig("more than 3 TCSes of one type"));
            }
        }

        let mut groups = [TcsGroup::default(); TCS_TYPE_NR];
        let mut st = 0u32;
        for &(ty, n) in &cfg.tcs_config {
            let g = &mut groups[ty as usize];
            if g.configured {
                return Err(RscError::BadConfig("TCS type listed twice"));
            }
            g.configured = true;
            g.num_tcs = n;
            g.ncpt = ncpt;
            if n == 0 || ty as usize == CONTROL_TCS {
                continue;
            }
            // `st + n >= BITS_PER_BYTE * sizeof(tcs->mask)`
            if st + n > max_tcs || st + n >= 32 {
                return Err(RscError::BadConfig(
                    "TCS groups need more TCSes than the DRV has",
                ));
            }
            g.mask = ((1u32 << n) - 1) << st;
            g.offset = st;
            st += n;
        }

        // The whole register window the geometry implies must be inside the mapping.
        let mut last = u64::from(cfg.tcs_offset) + u64::from(reg::IRQ_CLEAR) + 4;
        if st > 0 {
            last = u64::from(cfg.tcs_offset)
                + u64::from(reg::TCS_OFFSET) * u64::from(st - 1)
                + u64::from(reg::CMD_OFFSET) * u64::from(ncpt - 1)
                + u64::from(reg::CMD_STATUS)
                + 4;
        }
        if last > u64::from(cfg.window) {
            return Err(RscError::BadConfig(
                "TCS registers are outside the mapped window",
            ));
        }

        let rsc = Rsc {
            io,
            version: (major, minor),
            tcs_base: cfg.tcs_offset,
            groups,
            num_tcs: st,
            in_use: 0,
            bus: cfg.bus,
            ctrl_slots: [0; 2],
            quirks: Quirks::default(),
        };
        let held = Held::take(rsc.bus, None);
        rsc.io
            .write32(rsc.tcs_base + reg::IRQ_ENABLE, groups[ACTIVE_TCS].mask);
        drop(held);
        Ok(rsc)
    }

    /// `(major, minor)` of `RSC_DRV_ID`.
    pub fn version(&self) -> (u32, u32) {
        self.version
    }

    /// Group `ty` ([`ACTIVE_TCS`], ...).
    pub fn group(&self, ty: usize) -> TcsGroup {
        self.groups[ty]
    }

    /// How many TCSes the DRV has in its sleep, wake and active groups (`drv->num_tcs`).
    pub fn num_tcs(&self) -> u32 {
        self.num_tcs
    }

    /// Bit mask of TCSes this driver has claimed and not yet reclaimed (`drv->tcs_in_use`).
    pub fn in_use(&self) -> u32 {
        self.mask()
    }

    fn mask(&self) -> u32 {
        self.bus.map_or(self.in_use, Bus::in_use)
    }

    /// Changes the in-use mask. Called with the bus's lock held.
    fn set_mask(&mut self, set: u32, clear: u32) {
        match self.bus {
            Some(bus) => {
                let mask = (bus.in_use.load(Ordering::Acquire) | set) & !clear;
                bus.in_use.store(mask, Ordering::Release);
            }
            None => self.in_use = (self.in_use | set) & !clear,
        }
    }

    /// Every command slot a TCS has enabled, of every group: `(tcs, slot, address, data)`. Reads only. What another driver or the
    /// firmware before us left in a sleep or wake TCS is what it wanted while asleep or awake; an active TCS holds the last command
    /// it sent.
    pub fn slots(&self) -> alloc::vec::Vec<(u32, u32, u32, u32)> {
        let mut out = alloc::vec::Vec::new();
        for g in self.groups {
            if !g.configured {
                continue;
            }
            for tcs in g.offset..g.offset + g.num_tcs {
                let enabled = self.io.read32(self.tcs_reg(reg::CMD_ENABLE, tcs));
                for slot in 0..g.ncpt.min(MAX_CMDS_PER_TCS as u32) {
                    if enabled & (1 << slot) != 0 {
                        let addr = self.io.read32(self.tcs_cmd(reg::CMD_ADDR, tcs, slot));
                        let data = self.io.read32(self.tcs_cmd(reg::CMD_DATA, tcs, slot));
                        out.push((tcs, slot, addr, data));
                    }
                }
            }
        }
        out
    }

    /// `tcs_reg_addr`. `tcs` is below 12 and `reg` below 0x44, and `probe` checked the window, so no overflow.
    fn tcs_reg(&self, reg: u32, tcs: u32) -> u32 {
        self.tcs_base + reg::TCS_OFFSET * tcs + reg
    }

    /// `tcs_cmd_addr`.
    fn tcs_cmd(&self, reg: u32, tcs: u32, cmd: u32) -> u32 {
        self.tcs_reg(reg, tcs) + reg::CMD_OFFSET * cmd
    }

    /// `write_tcs_reg_sync`: write, then read until the same value comes back.
    fn write_sync(&self, tcs: u32, register: u32, value: u32) -> Result<(), RscError> {
        let at = self.tcs_reg(register, tcs);
        self.io.write32(at, value);
        for _ in 0..SYNC_READBACK_POLLS {
            if self.io.read32(at) == value {
                return Ok(());
            }
        }
        Err(RscError::RegisterStuck { tcs, reg: register })
    }

    /// `get_tcs_for_msg` for the active state: the ACTIVE group, or the WAKE group when the DRV has no ACTIVE TCS.
    fn active_group(&self) -> Result<usize, RscError> {
        if self.groups[ACTIVE_TCS].num_tcs > 0 {
            Ok(ACTIVE_TCS)
        } else if self.groups[WAKE_TCS].num_tcs > 0 {
            Ok(WAKE_TCS)
        } else {
            Err(RscError::NoActiveTcs)
        }
    }

    /// `check_for_req_inflight`: is a command to one of `req`'s resources in a TCS this driver still has claimed?
    fn inflight_conflict(&self, g: &TcsGroup, req: &TcsRequest<'_>) -> bool {
        for tcs in g.offset..g.offset + g.num_tcs {
            if self.mask() & (1 << tcs) == 0 {
                continue;
            }
            let enabled = self.io.read32(self.tcs_reg(reg::CMD_ENABLE, tcs));
            for j in 0..MAX_CMDS_PER_TCS as u32 {
                if enabled & (1 << j) == 0 {
                    continue;
                }
                let addr = self.io.read32(self.tcs_cmd(reg::CMD_ADDR, tcs, j));
                if req.cmds.iter().any(|c| match_resource_addr(c.addr, addr)) {
                    return true;
                }
            }
        }
        false
    }

    /// `claim_tcs_for_req` with `find_free_tcs`.
    fn claim(&self, g: &TcsGroup, req: &TcsRequest<'_>) -> Result<u32, RscError> {
        if self.inflight_conflict(g, req) {
            return Err(RscError::Busy);
        }
        (g.offset..g.offset + g.num_tcs)
            .find(|tcs| self.mask() & (1 << tcs) == 0)
            .ok_or(RscError::Busy)
    }

    /// `enable_tcs_irq`.
    fn enable_tcs_irq(&self, tcs: u32, enable: bool) {
        let at = self.tcs_base + reg::IRQ_ENABLE;
        let mut data = self.io.read32(at);
        if enable {
            data |= 1 << tcs;
        } else {
            data &= !(1 << tcs);
        }
        self.io.write32(at, data);
    }

    /// `__tcs_buffer_write`. Returns the mask of response-required commands (for `CMD_WAIT_FOR_CMPL`).
    fn buffer_write(&self, tcs: u32, first_cmd: u32, req: &TcsRequest<'_>) -> u32 {
        let mut cmd_msgid = CMD_MSGID_LEN | CMD_MSGID_WRITE;
        // Convert all commands to response-required when the request has wait_for_compl set.
        if req.wait_for_compl {
            cmd_msgid |= CMD_MSGID_RESP_REQ;
        }
        let mut cmd_enable = 0u32;
        let mut resp_mask = 0u32;
        for (i, cmd) in req.cmds.iter().enumerate() {
            let j = first_cmd + i as u32;
            cmd_enable |= 1 << j;
            let mut msgid = cmd_msgid;
            // A command with `wait` is response-required even in a fire-and-forget request.
            if cmd.wait {
                msgid |= CMD_MSGID_RESP_REQ;
            }
            if msgid & CMD_MSGID_RESP_REQ != 0 {
                resp_mask |= 1 << j;
            }
            self.io.write32(self.tcs_cmd(reg::CMD_MSGID, tcs, j), msgid);
            self.io
                .write32(self.tcs_cmd(reg::CMD_ADDR, tcs, j), cmd.addr);
            self.io
                .write32(self.tcs_cmd(reg::CMD_DATA, tcs, j), cmd.data);
        }
        cmd_enable |= self.io.read32(self.tcs_reg(reg::CMD_ENABLE, tcs));
        self.io
            .write32(self.tcs_reg(reg::CMD_ENABLE, tcs), cmd_enable);
        resp_mask
    }

    /// `__tcs_set_trigger`: untrigger and disable, then (for `trigger`) enable the AMC mode and trigger.
    ///
    /// The hardware requires the trigger bit cleared before the enable bit when clearing, and the enable bit set
    /// (and visible) before the trigger bit when starting. Only the last write is not read back.
    fn set_trigger(&self, tcs: u32, trigger: bool) -> Result<(), RscError> {
        let control = reg::CONTROL;
        let mut enable = self.io.read32(self.tcs_reg(control, tcs));
        enable &= !TCS_AMC_MODE_TRIGGER;
        self.write_sync(tcs, control, enable)?;
        enable &= !TCS_AMC_MODE_ENABLE;
        self.write_sync(tcs, control, enable)?;

        if trigger {
            enable = TCS_AMC_MODE_ENABLE;
            self.write_sync(tcs, control, enable)?;
            enable |= TCS_AMC_MODE_TRIGGER;
            self.io.write32(self.tcs_reg(control, tcs), enable);
        }
        Ok(())
    }

    /// Wait for the TCS's bit in `IRQ_STATUS`: [`SPIN_POLLS`] reads, then one read per millisecond of `delay`.
    fn wait_for_irq(&self, tcs: u32, delay: &impl Delay, timeout_ms: u32) -> bool {
        let at = self.tcs_base + reg::IRQ_STATUS;
        let bit = 1u32 << tcs;
        for _ in 0..SPIN_POLLS {
            if self.io.read32(at) & bit != 0 {
                return true;
            }
        }
        for _ in 0..timeout_ms {
            delay.sleep_ms(1);
            if self.io.read32(at) & bit != 0 {
                return true;
            }
        }
        false
    }

    /// The part of `tcs_tx_done` after the interrupt status was read, for one TCS: untrigger, disable the commands,
    /// clear the status bit, give the TCS back, and switch the interrupt off again when the TCS is borrowed.
    fn reclaim(&mut self, tcs: u32) -> Result<(), RscError> {
        self.set_trigger(tcs, false)?;
        if self.quirks.write_wait_for_cmpl {
            self.io
                .write32(self.tcs_reg(reg::CMD_WAIT_FOR_CMPL, tcs), 0);
        }
        self.io.write32(self.tcs_reg(reg::CMD_ENABLE, tcs), 0);
        self.io.write32(self.tcs_base + reg::IRQ_CLEAR, 1 << tcs);
        self.set_mask(0, 1 << tcs);
        // "Disable interrupt for WAKE TCS to avoid being spammed with interrupts coming when the solver sends its
        // wake votes."
        if self.groups[ACTIVE_TCS].num_tcs == 0 {
            self.enable_tcs_irq(tcs, false);
        }
        Ok(())
    }

    /// The first command of `req` whose status shows it did not get through, read before the TCS is cleared.
    fn bad_status(&self, tcs: u32, req: &TcsRequest<'_>) -> Option<(u32, u32)> {
        for (j, cmd) in req.cmds.iter().enumerate() {
            let status = self.io.read32(self.tcs_cmd(reg::CMD_STATUS, tcs, j as u32));
            let resp = req.wait_for_compl || cmd.wait;
            let want = CMD_STATUS_ISSUED | if resp { CMD_STATUS_COMPL } else { 0 };
            if status & want != want {
                return Some((j as u32, status));
            }
        }
        None
    }

    /// `rpmh_rsc_send_data` for an active-only request, then the wait and `tcs_tx_done` in place of the interrupt.
    ///
    /// `timeout_ms` is how long to sleep (in 1 ms steps of `delay`) after the first [`SPIN_POLLS`] reads found nothing.
    /// On [`RscError::Timeout`] and on a stuck register the TCS stays claimed, because its state is unknown.
    pub fn send(
        &mut self,
        req: &TcsRequest<'_>,
        delay: &impl Delay,
        timeout_ms: u32,
    ) -> Result<(), RscError> {
        let kind = self.active_group()?;
        let g = self.groups[kind];
        if req.cmds.is_empty() {
            return Err(RscError::EmptyRequest);
        }
        let max = (g.ncpt as usize).min(MAX_CMDS_PER_TCS);
        if req.cmds.len() > max {
            return Err(RscError::TooManyCmds { max });
        }
        if let Some(index) = req.cmds.iter().position(|c| c.addr == 0) {
            return Err(RscError::BadAddress { index });
        }

        let _held = Held::take(self.bus, Some(delay));
        let tcs = self.claim(&g, req)?;
        self.set_mask(1 << tcs, 0);
        if kind != ACTIVE_TCS {
            // Clear previously programmed WAKE commands in the repurposed TCS to avoid triggering them.
            self.write_sync(tcs, reg::CMD_ENABLE, 0)?;
            self.enable_tcs_irq(tcs, true);
        }

        let resp_mask = self.buffer_write(tcs, 0, req);
        if self.quirks.write_wait_for_cmpl {
            self.write_sync(tcs, reg::CMD_WAIT_FOR_CMPL, resp_mask)?;
        }
        self.set_trigger(tcs, true)?;

        if !self.wait_for_irq(tcs, delay, timeout_ms) {
            return Err(RscError::Timeout { tcs });
        }
        let bad = if self.quirks.check_cmd_status {
            self.bad_status(tcs, req)
        } else {
            None
        };
        self.reclaim(tcs)?;
        match bad {
            Some((cmd, status)) => Err(RscError::CmdNotAcked { tcs, cmd, status }),
            None => Ok(()),
        }
    }

    /// Send one command as `rpmh_write` (`cmd.wait`) or `rpmh_write_async` (no `cmd.wait`) would.
    pub fn send_cmd(
        &mut self,
        cmd: &TcsCmd,
        delay: &impl Delay,
        timeout_ms: u32,
    ) -> Result<(), RscError> {
        let mut one = [TcsCmd::default(); 1];
        let req = cmd.as_request(&mut one);
        self.send(&req, delay, timeout_ms)
    }

    /// `tcs_invalidate` of both groups, as `rpmh_rsc_invalidate`: only a group that has been written.
    pub fn invalidate_ctrl(&mut self) -> Result<(), RscError> {
        let _held = Held::take(self.bus, None);
        for ty in [SLEEP_TCS, WAKE_TCS] {
            let g = self.groups[ty];
            if self.ctrl_slots[ty] == 0 {
                continue;
            }
            for tcs in g.offset..g.offset + g.num_tcs {
                self.write_sync(tcs, reg::CMD_ENABLE, 0)?;
            }
            self.ctrl_slots[ty] = 0;
        }
        Ok(())
    }

    /// `rpmh_rsc_write_ctrl_data` for one command: `find_slots` and `__tcs_buffer_write`, without a trigger. The hardware
    /// runs these commands by itself when the application processor's cluster sleeps or wakes.
    pub fn write_ctrl(
        &mut self,
        ty: usize,
        addr: u32,
        data: u32,
        wait_for_compl: bool,
    ) -> Result<(), RscError> {
        let _held = Held::take(self.bus, None);
        let g = self.groups[ty];
        let slot = self.ctrl_slots[ty].trailing_ones();
        if slot >= g.num_tcs * g.ncpt {
            return Err(RscError::Busy);
        }
        let cmds = [TcsCmd {
            addr,
            data,
            wait: false,
        }];
        let req = TcsRequest {
            cmds: &cmds,
            wait_for_compl,
        };
        self.ctrl_slots[ty] |= 1 << slot;
        self.buffer_write(g.offset + slot / g.ncpt, slot % g.ncpt, &req);
        Ok(())
    }

    /// What `tcs_tx_done` would do now for every claimed TCS whose status bit is set: for requests that
    /// timed out and completed later. Returns how many TCSes were given back.
    pub fn reap(&mut self) -> u32 {
        let _held = Held::take(self.bus, None);
        let status = self.io.read32(self.tcs_base + reg::IRQ_STATUS);
        let mut reaped = 0;
        for tcs in 0..self.num_tcs {
            if self.mask() & (1 << tcs) != 0
                && status & (1 << tcs) != 0
                && self.reclaim(tcs).is_ok()
            {
                reaped += 1;
            }
        }
        reaped
    }
}
