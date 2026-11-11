//! The host controller: probe, bring-up, and the requests the block device needs.
//!
//! Bring-up follows Linux `ufshcd_hba_enable` / `ufshcd_link_startup` / `ufshcd_make_hba_operational` /
//! `ufshcd_verify_dev_init` / `ufshcd_complete_dev_init`, with one addition the laptop needs: when the firmware left
//! the controller enabled with a device on the link (the Galaxy Book Go's UEFI boots from this disk), the driver
//! first tries to keep that link and only points the request lists at its own memory. A full reset of a Qualcomm
//! controller needs its PHY, clocks and regulators brought up again (`ufs_qcom_hce_enable_notify`,
//! `ufs_qcom_power_up_sequence`), which LucinaOS cannot do yet, so the kernel passes [`ResetPolicy::Never`] there.

use alloc::vec::Vec;
use core::fmt;

use crate::{
    Error,
    cache::CacheReport,
    desc::{self, DeviceDesc, GeometryDesc, UnitDesc},
    irq_wait,
    policy::{UnitRole, WritePolicy},
    regs::{self, Capabilities},
    scsi::{self, Capacity, Inquiry, ModeSense},
    upiu::{self, DataDir, Query, UPIU_AREA, UPIU_HEADER_AND_TSF, UTRD_SIZE, Utrd},
};

/// MMIO access to the controller's registers (32-bit, little endian, offsets from [`regs`]).
pub trait Regs {
    fn read32(&self, offset: usize) -> u32;
    fn write32(&self, offset: usize, value: u32);
}

/// The driver's own DMA memory: [`AREA_SIZE`] bytes the controller sees at [`DmaArea::daddr`], 1 KiB aligned.
pub trait DmaArea {
    fn daddr(&self) -> u64;
    fn read(&self, offset: usize, buf: &mut [u8]);
    fn write(&self, offset: usize, buf: &[u8]);
    /// Called after the CPU wrote `len` bytes at `offset` and before the controller may read them.
    fn sync_to_device(&self, _offset: usize, _len: usize) {}
    /// Called after the controller wrote `len` bytes at `offset` and before the CPU reads them.
    fn sync_from_device(&self, _offset: usize, _len: usize) {}
}

/// Time for timeouts and polling delays.
pub trait Clock {
    fn now_us(&self) -> u64;
    /// Waits about `us` microseconds (may yield to other threads).
    fn delay_us(&self, us: u64);

    /// Sleeps until the controller interrupts for a request (`IS.UTRCS`, `IS.UE` or a fatal bit; Linux `ufshcd_intr`), for
    /// at most `timeout_us` (the host asks for at most [`crate::irq_wait::SLEEP_CAP_US`]). [`IrqWait::Unavailable`]: there is
    /// no interrupt to sleep on here (no line, or a caller that may not sleep), and the caller polls.
    fn wait_interrupt(&self, _timeout_us: u64) -> IrqWait {
        IrqWait::Unavailable
    }
    /// A request finished although no interrupt woke its thread: found `late_us` at most after it finished. For the
    /// platform's own count (the kernel's `/proc/lucina-irq`); the host counts and logs it too.
    fn missed_interrupt(&self, _late_us: u64) {}
    /// One line of the bring-up log, at most about 75 columns: on the laptop it is read on a screen or from
    /// `/dev/kmsg` at 100 columns, after the kernel's timestamp and prefix.
    fn note(&self, _line: fmt::Arguments<'_>) {}
    /// A request failed: logs what only the platform can see, while the request is still pending if it is
    /// (clocks, the IOMMU).
    fn platform_report(&self) {}
}

/// How [`Clock::wait_interrupt`] ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IrqWait {
    /// No interrupt to sleep on: the caller polls.
    Unavailable,
    /// The interrupt came (the handler saw one of the bits a request waits for).
    Interrupted,
    /// The sleep ended at its timeout with no interrupt.
    Expired,
}

/// Layout of the DMA area.
pub const AREA_SIZE: usize = 0x4000;
/// UTP transfer request list (32 slots of 32 bytes; only slot 0 is used). 1 KiB aligned, as UTRLBA requires.
pub const UTRL_OFFSET: usize = 0x0000;
/// UTP task management request list (unused, but its base must point at memory the driver owns).
pub const UTMRL_OFFSET: usize = 0x0400;
/// Command descriptor of slot 0: command UPIU, response UPIU, PRDT. 128-byte aligned.
pub const UCD_OFFSET: usize = 0x1000;
pub const RSP_OFFSET: usize = UPIU_AREA;
pub const PRDT_OFFSET: usize = 2 * UPIU_AREA;
/// PRDT entries that fit before the bounce buffer: 192, i.e. at least 768 KiB of 4 KiB pages per request.
pub const PRDT_MAX: usize = (BOUNCE_OFFSET - UCD_OFFSET - PRDT_OFFSET) / upiu::PRDT_ENTRY_SIZE;
/// A buffer of the driver's own for small transfers (INQUIRY, READ CAPACITY) and single blocks.
pub const BOUNCE_OFFSET: usize = 0x2000;
pub const BOUNCE_SIZE: usize = AREA_SIZE - BOUNCE_OFFSET;

/// Allocation length of a MODE SENSE (10) for one page: header, a long block descriptor and the 20-byte Caching page
/// fit many times over; a multiple of 4, as a PRDT entry must be.
const MODE_SENSE_ALLOC: usize = 0x100;

/// The task tag; one request at a time, always in slot 0.
const TAG: u8 = 0;
const SLOT0: u32 = 1;

/// Linux values (`ufshcd.c`): NOP OUT 50 ms x 10, link startup x 3, fDeviceInit 1.5 s, UIC commands 500 ms.
const NOP_TIMEOUT_US: u64 = 50_000;
const NOP_RETRIES: u32 = 10;
const LINK_STARTUP_RETRIES: u32 = 3;
const DEVICE_INIT_TIMEOUT_US: u64 = 1_500_000;
const UIC_TIMEOUT_US: u64 = 500_000;
const QUERY_TIMEOUT_US: u64 = 1_500_000;
/// SCSI commands: as Linux sd, 30 s; 60 s for a cache flush.
const SCSI_TIMEOUT_US: u64 = 30_000_000;
const FLUSH_TIMEOUT_US: u64 = 60_000_000;
/// Reads and writes that the device asks to have repeated: `SD_MAX_RETRIES` (sd.h).
const RW_RETRIES: u32 = 5;
/// Query requests (descriptor, attribute and flag reads): `QUERY_REQ_RETRIES` (ufshcd.c).
const QUERY_RETRIES: u32 = 3;
const HCE_TIMEOUT_US: u64 = 50_000;
const CLEAR_TIMEOUT_US: u64 = 1_000_000;
/// How long the firmware's last requests may still be in flight when the driver takes over.
const HANDOVER_TIMEOUT_US: u64 = 100_000;
/// Rounds of TEST UNIT READY while the unit says NOT READY, 1 s apart (sd's spin cycle sleeps 1 s). sd sends START
/// STOP UNIT first, which the command allowlist forbids, so a unit that stays NOT READY is not spun up: 5 rounds, ours.
const READY_RETRIES: u32 = 5;
const READY_DELAY_US: u64 = 1_000_000;
/// `sd_spinup_disk`'s `scsi_failure`: a TEST UNIT READY that does not pass is sent again up to 3 times at once.
const TUR_RETRIES: u32 = 3;
/// `READ_CAPACITY_RETRIES_ON_RESET` of sd.c.
const READ_CAPACITY_RETRIES_ON_RESET: u32 = 10;
/// Failed requests described in the log per attempt at the device, so ten NOP OUTs do not fill the screen.
const REQUEST_NOTES: u32 = 3;
/// Most units a UFS 2.x/3.x device has (bMaxNumberLU = 0); 32 with bMaxNumberLU = 1.
const DEFAULT_MAX_LU: u8 = 8;

/// What to do when the controller is not usable as the firmware left it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResetPolicy {
    /// Only reuse the firmware's link; fail otherwise (Qualcomm hosts until LucinaOS can drive their PHY).
    Never,
    /// Reuse the firmware's link if there is one that answers, else reset and bring the link up.
    IfNeeded,
    /// Always reset and bring the link up (generic UFSHCI, QEMU).
    Always,
}

/// The registers as found before the driver wrote anything. Reading them has no side effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Probe {
    pub cap: Capabilities,
    pub version: u32,
    pub hcpid: u32,
    pub hcmid: u32,
    pub ahit: u32,
    /// UTP transfer request interrupt aggregation control, read once at probe and never written (Linux writes 0 when
    /// aggregation is not allowed; with IE = 0 and doorbell polling the value cannot matter, but the log shows what
    /// the firmware left).
    pub utrilacr: u32,
    pub is: u32,
    pub ie: u32,
    pub hcs: u32,
    pub hce: u32,
    pub utrlba: u64,
    pub utrldbr: u32,
    pub utrlrsr: u32,
    pub utmrlba: u64,
    pub utmrldbr: u32,
    pub utmrlrsr: u32,
    /// The last UIC command and its arguments (result in UCMDARG2), as the firmware left them.
    pub uiccmd: u32,
    pub ucmdarg: [u32; 3],
}

impl Probe {
    pub fn read(r: &impl Regs) -> Self {
        let lohi = |lo, hi| r.read32(lo) as u64 | (r.read32(hi) as u64) << 32;
        Self {
            cap: Capabilities::new(r.read32(regs::CAP)),
            version: r.read32(regs::VER),
            hcpid: r.read32(regs::HCPID),
            hcmid: r.read32(regs::HCMID),
            ahit: r.read32(regs::AHIT),
            utrilacr: r.read32(regs::UTRIACR),
            is: r.read32(regs::IS),
            ie: r.read32(regs::IE),
            hcs: r.read32(regs::HCS),
            hce: r.read32(regs::HCE),
            utrlba: lohi(regs::UTRLBA, regs::UTRLBAU),
            utrldbr: r.read32(regs::UTRLDBR),
            utrlrsr: r.read32(regs::UTRLRSR),
            utmrlba: lohi(regs::UTMRLBA, regs::UTMRLBAU),
            utmrldbr: r.read32(regs::UTMRLDBR),
            utmrlrsr: r.read32(regs::UTMRLRSR),
            uiccmd: r.read32(regs::UICCMD),
            ucmdarg: [
                r.read32(regs::UCMDARG1),
                r.read32(regs::UCMDARG2),
                r.read32(regs::UCMDARG3),
            ],
        }
    }

    /// Enabled by the firmware with a device on the link: a candidate for reuse.
    pub fn link_up(&self) -> bool {
        self.hce & regs::HCE_ENABLE != 0 && self.hcs & regs::HCS_DP != 0
    }
}

/// The link as the UniPro and M-PHY attributes describe it; `None` where DME_GET failed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LinkState {
    /// PA_PWRMODE: RX mode in bits 7:4, TX mode in bits 3:0 (1 fast, 2 slow, 4 fast auto, 5 slow auto).
    pub pwrmode: Option<u32>,
    pub rx_gear: Option<u32>,
    pub tx_gear: Option<u32>,
    pub rx_lanes: Option<u32>,
    pub tx_lanes: Option<u32>,
    pub tx_fsm: Option<u32>,
    pub rx_fsm: Option<u32>,
}

impl LinkState {
    pub fn hibernating(&self) -> bool {
        self.tx_fsm == Some(regs::TX_FSM_HIBERN8)
    }
}

/// An attribute value or `?`, for the log.
struct Attr(Option<u32>);

impl fmt::Display for Attr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(v) => write!(f, "{v:#x}"),
            None => f.write_str("?"),
        }
    }
}

/// Choices for taking over from the firmware.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options {
    /// Clear run-stop before moving the request lists, and set it again. Without it the bases are written with the
    /// lists left running. Neither is what Linux does (it resets first), so the laptop decides.
    pub restart_lists: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            restart_lists: true,
        }
    }
}

/// How the link came up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Started {
    /// The firmware's link, untouched apart from the request list bases.
    Reused,
    /// Controller reset, link startup, fDeviceInit.
    Reset,
}

/// One data buffer of a request, as the controller addresses it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DataSegment {
    pub daddr: u64,
    pub len: usize,
}

/// A logical unit found at probe time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Unit {
    pub lun: u8,
    pub desc: UnitDesc,
    pub role: UnitRole,
    pub block_size: u32,
    pub blocks: u64,
    pub inquiry: Option<Inquiry>,
}

impl Unit {
    pub fn bytes(&self) -> u64 {
        self.blocks.saturating_mul(self.block_size as u64)
    }
}

pub struct Host<R: Regs, D: DmaArea, C: Clock> {
    regs: R,
    area: D,
    clock: C,
    probe: Probe,
    started: Started,
    policy: WritePolicy,
    device: Option<DeviceDesc>,
    geometry: Option<GeometryDesc>,
    units: Vec<Unit>,
    /// Set when a request could not be taken back from the controller; every later request fails.
    dead: bool,
    /// Bytes of caller memory (outside this host's area) that the PRDT of the request which ended the host named: the
    /// controller may still read or write them, so the caller must keep them for good ([`Self::keep_held`]).
    held: usize,
    /// How many more failed requests get a line in the log.
    request_notes: u32,
    options: Options,
    /// The controller's lists point at this host's DMA area.
    lists_ours: bool,
    /// The timing of the last request, for the log.
    timing: Timing,
    /// The first request that worked has been described.
    first_done: bool,
    /// Requests that finished although no interrupt woke their thread (found after a capped sleep).
    missed_irq: u64,
    /// The most a missed interrupt's request was found late, in milliseconds (an upper bound: the sleep's length).
    late_ms_max: u64,
    /// Rate limit of the log lines about missed interrupts.
    miss_notes: crate::irq_wait::MissNotes,
}

/// When a request's door bell cleared and its descriptor changed, in microseconds after the door bell.
#[derive(Clone, Copy, Debug, Default)]
struct Timing {
    /// UTRLDBR read right after it was written: 1 if the controller took the bell at once.
    first_read: u32,
    dbr_clear_us: Option<u64>,
    ocs_us: Option<u64>,
}

impl<R: Regs, D: DmaArea, C: Clock> Host<R, D, C> {
    /// Brings the controller up as `reset` allows, reads the descriptors and finds the logical units.
    pub fn start(
        regs: R,
        area: D,
        clock: C,
        reset: ResetPolicy,
        policy: WritePolicy,
    ) -> Result<Self, Error> {
        Self::start_with(regs, area, clock, reset, policy, Options::default())
    }

    /// [`Self::start`] with choices. After a failure the DMA area is never freed if the controller's lists point at
    /// it: a late completion would write into pages the kernel has given to something else.
    pub fn start_with(
        regs: R,
        area: D,
        clock: C,
        reset: ResetPolicy,
        policy: WritePolicy,
        options: Options,
    ) -> Result<Self, Error> {
        let probe = Probe::read(&regs);
        clock.note(format_args!(
            "probe: AHIT {:#x} UTRIACR {:#x}",
            probe.ahit, probe.utrilacr
        ));
        if !area.daddr().is_multiple_of(1024)
            || (!probe.cap.addr64 && area.daddr() + AREA_SIZE as u64 > 1 << 32)
        {
            return Err(Error::BadBuffer);
        }
        let mut host = Self {
            regs,
            area,
            clock,
            probe,
            started: Started::Reused,
            policy,
            device: None,
            geometry: None,
            units: Vec::new(),
            dead: false,
            held: 0,
            request_notes: REQUEST_NOTES,
            options,
            lists_ours: false,
            timing: Timing::default(),
            first_done: false,
            missed_irq: 0,
            late_ms_max: 0,
            miss_notes: Default::default(),
        };
        let reused = match reset {
            ResetPolicy::Always => Err(Error::NotEnabled),
            _ if !probe.link_up() => Err(if probe.hce & regs::HCE_ENABLE == 0 {
                Error::NotEnabled
            } else {
                Error::NoDevice
            }),
            _ => host.reuse(),
        };
        match (reused, reset) {
            (Ok(()), _) => host.started = Started::Reused,
            (Err(e), ResetPolicy::Never) => return Err(host.give_up(e)),
            (Err(_), _) => {
                if let Err(e) = host.full_init() {
                    return Err(host.give_up(e));
                }
                host.started = Started::Reset;
            }
        }
        if let Err(e) = host.scan() {
            return Err(host.give_up(e));
        }
        Ok(host)
    }

    /// Bring-up failed: the lists stop, and if the controller ever pointed at this host's memory, that memory stays
    /// allocated for good.
    fn give_up(mut self, error: Error) -> Error {
        if self.lists_ours {
            self.stop_lists();
            core::mem::forget(self);
        }
        error
    }

    fn stop_lists(&mut self) {
        self.regs.write32(regs::UTRLRSR, 0);
        self.regs.write32(regs::UTMRLRSR, 0);
    }

    /// Stops the controller's use of this host's memory, for a jump to another kernel: that kernel finds the link up
    /// and the lists stopped. Requests in flight would be lost, but one thread runs them and it is stopped by then.
    pub fn quiesce(&mut self) {
        let _ = self.wait_reg(regs::UTRLDBR, SLOT0, 0, CLEAR_TIMEOUT_US, "slot 0");
        self.stop_lists();
        self.dead = true;
    }

    pub fn probe(&self) -> &Probe {
        &self.probe
    }

    pub fn started(&self) -> Started {
        self.started
    }

    pub fn device(&self) -> Option<&DeviceDesc> {
        self.device.as_ref()
    }

    pub fn geometry(&self) -> Option<&GeometryDesc> {
        self.geometry.as_ref()
    }

    pub fn units(&self) -> &[Unit] {
        &self.units
    }

    pub fn unit(&self, lun: u8) -> Option<&Unit> {
        self.units.iter().find(|u| u.lun == lun)
    }

    pub fn regs(&self) -> &R {
        &self.regs
    }

    /// Requests that finished although no interrupt woke their thread, since start.
    pub fn missed_irq(&self) -> u64 {
        self.missed_irq
    }

    /// The most a request with a missed interrupt was found late, in milliseconds (at most the capped sleep's length).
    pub fn late_ms_max(&self) -> u64 {
        self.late_ms_max
    }

    /// Whether an earlier request ended the host: nothing more goes to the controller.
    pub fn is_dead(&self) -> bool {
        self.dead
    }

    /// After a failed request: if the controller was given caller memory and never proved it finished with it (its
    /// slot would not clear, or it was cleared with no ABORT TASK sent to the device, which may still hold task tag 0,
    /// or a fatal error ended the host mid-transfer), moves `buffers` (the owners of that request's data segments)
    /// into `quarantine`, which the caller keeps for good, and returns the bytes kept. Otherwise `buffers` is not
    /// consumed and 0 is returned. Each request is quarantined once.
    pub fn keep_held<T>(
        &mut self,
        buffers: impl IntoIterator<Item = T>,
        quarantine: &mut Vec<T>,
    ) -> usize {
        let held = core::mem::take(&mut self.held);
        if held > 0 {
            quarantine.extend(buffers);
        }
        held
    }

    pub fn area(&self) -> &D {
        &self.area
    }

    // ---- bring-up ----

    /// Keeps the firmware's link: waits for its last requests, reads the link's state, points the lists at our
    /// area, pings the device. Hibernate exit only when the link is hibernating (Linux decides from the TX FSM,
    /// `ufs_qcom_check_hibern8`), or when its state could not be read and the ping failed; it failing does not end
    /// the bring-up, because on an active link it has nothing to do. The error returned is the ping's.
    fn reuse(&mut self) -> Result<(), Error> {
        self.wait_reg(
            regs::UTRLDBR,
            u32::MAX,
            0,
            HANDOVER_TIMEOUT_US,
            "the firmware's transfer requests",
        )
        .or_else(|_| self.clear_leftover_requests())?;
        self.wait_reg(
            regs::UTMRLDBR,
            u32::MAX,
            0,
            HANDOVER_TIMEOUT_US,
            "the firmware's task requests",
        )?;
        let link = self.link_state();
        self.note_uic_errors();
        if link.hibernating() {
            self.hibernate_exit()?;
        } else {
            self.note_peer();
        }
        self.make_operational()?;
        let mut result = self.verify_device();
        if result.is_err() && link.tx_fsm.is_none() {
            self.hibernate_exit()?;
            self.request_notes = REQUEST_NOTES;
            result = self.verify_device();
        }
        if let Err(e) = result {
            self.post_mortem();
            return Err(e);
        }
        self.clock.note(format_args!("NOP OUT answered"));
        // The firmware finished device initialization long ago; a device still in it is waited for, not restarted.
        self.wait_device_init()
    }

    /// Requests another kernel or the firmware left in the transfer list: logged, then cleared.
    fn clear_leftover_requests(&mut self) -> Result<(), Error> {
        let dbr = self.regs.read32(regs::UTRLDBR);
        self.clock
            .note(format_args!("takeover: DBR {dbr:#x} still set, clearing"));
        self.regs.write32(regs::UTRLCLR, !dbr);
        self.wait_reg(
            regs::UTRLDBR,
            u32::MAX,
            0,
            CLEAR_TIMEOUT_US,
            "leftover requests",
        )
    }

    /// Reads and logs the link's state. Only DME_GETs, nothing changes.
    pub fn link_state(&mut self) -> LinkState {
        let mut failure = None;
        let mut get = |host: &mut Self, arg: u32| match host.uic(regs::UIC_DME_GET, arg, 0, 0) {
            Ok(v) => Some(v),
            Err(e) => {
                failure.get_or_insert(e);
                None
            }
        };
        let tx_fsm_sel = regs::uic_arg_mib_sel(regs::MPHY_TX_FSM_STATE, 0);
        let rx_fsm_sel = regs::uic_arg_mib_sel(regs::MPHY_RX_FSM_STATE, regs::RX_LANE0_SEL);
        let link = LinkState {
            pwrmode: get(self, regs::uic_arg_mib(regs::PA_PWRMODE)),
            rx_gear: get(self, regs::uic_arg_mib(regs::PA_RXGEAR)),
            tx_gear: get(self, regs::uic_arg_mib(regs::PA_TXGEAR)),
            rx_lanes: get(self, regs::uic_arg_mib(regs::PA_CONNECTEDRXDATALANES)),
            tx_lanes: get(self, regs::uic_arg_mib(regs::PA_CONNECTEDTXDATALANES)),
            tx_fsm: get(self, tx_fsm_sel),
            rx_fsm: get(self, rx_fsm_sel),
        };
        self.clock.note(format_args!(
            "link: PWRMODE {} gear rx {} tx {} lanes rx {} tx {}",
            Attr(link.pwrmode),
            Attr(link.rx_gear),
            Attr(link.tx_gear),
            Attr(link.rx_lanes),
            Attr(link.tx_lanes),
        ));
        self.clock.note(format_args!(
            "link: TX FSM {} RX FSM {}: {}",
            Attr(link.tx_fsm),
            Attr(link.rx_fsm),
            match link.tx_fsm {
                None => "state unknown",
                Some(regs::TX_FSM_HIBERN8) => "hibernating",
                Some(_) => "not hibernating",
            }
        ));
        if let Some(e) = failure {
            self.clock.note(format_args!("link: first failure: {e}"));
        }
        link
    }

    /// Asks the device for an attribute over the link: tells a dead link from a problem in the transfer path.
    fn note_peer(&mut self) {
        let mut attempt = self.peer_get();
        for _ in 0..2 {
            if attempt.is_ok() {
                break;
            }
            self.clock.delay_us(1000);
            attempt = self.peer_get();
        }
        match attempt {
            Ok(v) => self.clock.note(format_args!(
                "device answers over the link: PA_TACTIVATE {v:#x}"
            )),
            Err(e) => self.clock.note(format_args!("device does not answer: {e}")),
        }
    }

    fn peer_get(&mut self) -> Result<u32, Error> {
        self.uic(
            regs::UIC_DME_PEER_GET,
            regs::uic_arg_mib(regs::PA_TACTIVATE),
            0,
            0,
        )
    }

    /// DME_HIBER_EXIT, as Linux `ufshcd_uic_pwr_ctrl`: the result, then IS.UHXS and HCS.UPMCRS. Linux requires
    /// UPMCRS == PWR_LOCAL afterwards and marks the link broken otherwise; here that fails the bring-up, as a
    /// failed hibernate exit does.
    fn hibernate_exit(&mut self) -> Result<(), Error> {
        self.regs
            .write32(regs::IS, regs::IS_UHXS | regs::IS_UHES | regs::IS_UPMS);
        match self.uic(regs::UIC_DME_HIBER_EXIT, 0, 0, 0) {
            Ok(_) => {
                let done = self.wait_reg(
                    regs::IS,
                    regs::IS_UHXS,
                    regs::IS_UHXS,
                    UIC_TIMEOUT_US,
                    "UHXS",
                );
                self.regs.write32(regs::IS, regs::IS_UHXS);
                let hcs = self.regs.read32(regs::HCS);
                let upmcrs = (hcs >> regs::HCS_UPMCRS_SHIFT) & regs::HCS_UPMCRS_MASK;
                self.clock.note(format_args!(
                    "hibernate exit: sent, UHXS {}, {}",
                    if done.is_ok() { "set" } else { "never set" },
                    regs::upmcrs_name(hcs),
                ));
                if upmcrs != regs::HCS_UPMCRS_PWR_LOCAL {
                    self.clock.note(format_args!(
                        "hibernate exit: UPMCRS {} is not PWR_LOCAL: link broken",
                        regs::upmcrs_name(hcs),
                    ));
                    return Err(Error::NoDevice);
                }
                Ok(())
            }
            Err(e) => {
                self.clock.note(format_args!("hibernate exit: {e}"));
                Ok(())
            }
        }
    }

    /// The UIC error registers. They clear when read, so `ufs=probe` does not look at them.
    fn note_uic_errors(&mut self) {
        let [pa, dl, n, t, dme] = [
            regs::UECPA,
            regs::UECDL,
            regs::UECN,
            regs::UECT,
            regs::UECDME,
        ]
        .map(|r| self.regs.read32(r));
        self.clock.note(format_args!(
            "UIC errors: PA {pa:#x} DL {dl:#x} N {n:#x} T {t:#x} DME {dme:#x}"
        ));
    }

    /// One failed request in the log, with what the controller and the descriptor show at that moment.
    fn note_request(&mut self, error: &Error) {
        if self.request_notes == 0 {
            return;
        }
        self.request_notes -= 1;
        self.area.sync_from_device(UTRL_OFFSET, UTRD_SIZE);
        self.area
            .sync_from_device(UCD_OFFSET + RSP_OFFSET, UPIU_AREA);
        let mut utrd = [0u8; UTRD_SIZE];
        self.area.read(UTRL_OFFSET, &mut utrd);
        let rsp = self.read_response();
        self.clock.note(format_args!("request failed: {error}"));
        self.note_timing("  ");
        self.clock.note(format_args!(
            "  DBR {:#x} IS {:#x} ({}) OCS {:#x}",
            self.regs.read32(regs::UTRLDBR),
            self.regs.read32(regs::IS),
            regs::IsNames(self.regs.read32(regs::IS)),
            upiu::ocs(&utrd),
        ));
        self.clock.note(format_args!(
            "  HCS {:#x} response UPIU {:02x} {:02x} {:02x} {:02x} resp {:02x} status {:02x}",
            self.regs.read32(regs::HCS),
            rsp[0],
            rsp[1],
            rsp[2],
            rsp[3],
            rsp[6],
            rsp[7],
        ));
        if *error == Error::Ocs(upiu::OCS_INVALID_COMMAND_STATUS) {
            self.clock.note(format_args!(
                "  slot given back, descriptor untouched: list base not taken?"
            ));
        }
        self.clock.platform_report();
    }

    fn note_timing(&self, indent: &str) {
        struct Us(Option<u64>);
        impl fmt::Display for Us {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                match self.0 {
                    Some(v) => write!(f, "+{v} us"),
                    None => f.write_str("never"),
                }
            }
        }
        self.clock.note(format_args!(
            "{indent}bell read back {:#x}, DBR clear {}, OCS set {}",
            self.timing.first_read,
            Us(self.timing.dbr_clear_us),
            Us(self.timing.ocs_us),
        ));
    }

    /// Everything else worth knowing after the device would not answer.
    fn post_mortem(&mut self) {
        let base =
            self.regs.read32(regs::UTRLBA) as u64 | (self.regs.read32(regs::UTRLBAU) as u64) << 32;
        self.clock.note(format_args!(
            "after failure: UTRLBA {base:#x} RSR {}/{} (driver's area {:#x})",
            self.regs.read32(regs::UTRLRSR),
            self.regs.read32(regs::UTMRLRSR),
            self.area.daddr(),
        ));
        self.note_uic_errors();
        self.note_peer();
    }

    /// Linux `ufshcd_hba_enable` + `ufshcd_link_startup` + operational lists + `ufshcd_complete_dev_init`.
    fn full_init(&mut self) -> Result<(), Error> {
        self.enable_controller()?;
        self.link_startup()?;
        // Linux: clear UECPA once, a LINERESET happens during link startup.
        let _ = self.regs.read32(regs::UECPA);
        self.make_operational()?;
        self.verify_device()?;
        self.query_flag(upiu::Query::set_flag(desc::FLAG_DEVICE_INIT))?;
        self.wait_device_init()
    }

    /// DME_LINKSTARTUP as Linux `ufshcd_link_startup`: a startup that answers but finds no device fails at once
    /// (`-ENXIO`); one that fails re-enables the HBA (`ufshcd_hba_enable`) and is tried again, up to
    /// `DME_LINKSTARTUP_RETRIES` tries. Linux's second pass runs only when `curr_dev_pwr_mode` is not active, and its
    /// first initialisation sets it active, so on this path (a first initialisation) there is none.
    fn link_startup(&mut self) -> Result<(), Error> {
        let mut last = Error::NoDevice;
        for attempt in 0..LINK_STARTUP_RETRIES {
            match self.uic(regs::UIC_DME_LINK_STARTUP, 0, 0, 0) {
                Ok(_) if self.regs.read32(regs::HCS) & regs::HCS_DP != 0 => return Ok(()),
                Ok(_) => return Err(Error::NoDevice),
                Err(e) => last = e,
            }
            if attempt + 1 < LINK_STARTUP_RETRIES {
                self.enable_controller()?;
            }
        }
        Err(last)
    }

    fn enable_controller(&mut self) -> Result<(), Error> {
        if self.regs.read32(regs::HCE) & regs::HCE_ENABLE != 0 {
            self.regs.write32(regs::HCE, 0);
            self.wait_reg(
                regs::HCE,
                regs::HCE_ENABLE,
                0,
                HCE_TIMEOUT_US,
                "HCE to clear",
            )?;
        }
        for _ in 0..3 {
            self.regs.write32(regs::HCE, regs::HCE_ENABLE);
            // "HCE goes 1 => 0 => 1": read too early and the old 1 is seen (Linux hba_enable_delay_us).
            self.clock.delay_us(1000);
            if self
                .wait_reg(
                    regs::HCE,
                    regs::HCE_ENABLE,
                    regs::HCE_ENABLE,
                    HCE_TIMEOUT_US,
                    "HCE",
                )
                .is_ok()
            {
                return Ok(());
            }
            self.regs.write32(regs::HCE, 0);
            self.clock.delay_us(1000);
        }
        Err(Error::Timeout("the host controller to enable"))
    }

    /// Request lists at our area, interrupts off (the driver polls), lists running.
    fn make_operational(&mut self) -> Result<(), Error> {
        self.area.write(0, &[0u8; UCD_OFFSET]);
        self.area.sync_to_device(0, UCD_OFFSET);
        let utrl = self.area.daddr() + UTRL_OFFSET as u64;
        let utmrl = self.area.daddr() + UTMRL_OFFSET as u64;
        self.regs.write32(regs::IE, 0);
        self.regs.write32(regs::IS, self.regs.read32(regs::IS));
        // Lists the firmware left running are stopped before their bases move: a controller that
        // takes the base only when run-stop goes from 0 to 1 would otherwise keep fetching
        // descriptors from the firmware's memory, which the kernel now uses for other things.
        let running = (
            self.regs.read32(regs::UTRLRSR),
            self.regs.read32(regs::UTMRLRSR),
        );
        if self.options.restart_lists {
            self.stop_lists();
        }
        self.clock.note(format_args!(
            "lists: RSR {}/{}, now {}/{}, restart {}",
            running.0,
            running.1,
            self.regs.read32(regs::UTRLRSR),
            self.regs.read32(regs::UTMRLRSR),
            self.options.restart_lists,
        ));
        self.regs.write32(regs::UTRLBA, utrl as u32);
        self.regs.write32(regs::UTRLBAU, (utrl >> 32) as u32);
        self.regs.write32(regs::UTMRLBA, utmrl as u32);
        self.regs.write32(regs::UTMRLBAU, (utmrl >> 32) as u32);
        let back =
            self.regs.read32(regs::UTRLBA) as u64 | (self.regs.read32(regs::UTRLBAU) as u64) << 32;
        if back != utrl {
            return Err(Error::BadData("UTRLBA read back"));
        }
        let ready = regs::HCS_UTRLRDY | regs::HCS_UTMRLRDY;
        if self.regs.read32(regs::HCS) & ready != ready {
            return Err(Error::Timeout("the request lists to be ready"));
        }
        // The task list first, as Linux ufshcd_enable_run_stop_reg.
        self.regs.write32(regs::UTMRLRSR, 1);
        self.regs.write32(regs::UTRLRSR, 1);
        self.lists_ours = true;
        self.clock.note(format_args!(
            "lists: at {utrl:#x}, UTRLBA reads {back:#x}, RSR {}/{}",
            self.regs.read32(regs::UTRLRSR),
            self.regs.read32(regs::UTMRLRSR),
        ));
        Ok(())
    }

    /// NOP OUT until the device answers NOP IN (Linux `ufshcd_verify_dev_init`). Linux stops at the first timeout
    /// (`break` on `-ETIMEDOUT`); so do we.
    fn verify_device(&mut self) -> Result<(), Error> {
        let mut last = Error::NoDevice;
        for _ in 0..NOP_RETRIES {
            match self.nop() {
                Ok(()) => return Ok(()),
                Err(Error::Dead) => {
                    return Err(if last == Error::NoDevice {
                        Error::Dead
                    } else {
                        last
                    });
                }
                Err(e @ Error::Timeout(_)) => {
                    self.note_request(&e);
                    return Err(e);
                }
                Err(e) => {
                    self.note_request(&e);
                    last = e;
                }
            }
        }
        Err(last)
    }

    fn nop(&mut self) -> Result<(), Error> {
        self.exec(&upiu::nop_out(TAG), DataDir::None, &[], NOP_TIMEOUT_US)?;
        let rsp = self.read_response();
        let h = upiu::parse_header(&rsp)?;
        if h.tt != upiu::TT_NOP_IN {
            return Err(Error::BadResponse {
                expected: upiu::TT_NOP_IN,
                got: h.tt,
            });
        }
        if h.response != 0 {
            return Err(Error::Response(h.response));
        }
        Ok(())
    }

    fn wait_device_init(&mut self) -> Result<(), Error> {
        let start = self.clock.now_us();
        loop {
            let (_, value) = self.query_flag(Query::read_flag(desc::FLAG_DEVICE_INIT))?;
            if value & 1 == 0 {
                return Ok(());
            }
            if self.clock.now_us().wrapping_sub(start) > DEVICE_INIT_TIMEOUT_US {
                return Err(Error::Timeout("fDeviceInit to clear"));
            }
            self.clock.delay_us(5000);
        }
    }

    /// Device, geometry and unit descriptors, then each enabled unit's capacity and identity.
    fn scan(&mut self) -> Result<(), Error> {
        let device = DeviceDesc::parse(&self.read_desc(desc::IDN_DEVICE, 0)?)?;
        self.device = Some(device);
        match self.read_attr(desc::ATTR_CURRENT_POWER_MODE, 0) {
            Ok(mode) => self.clock.note(format_args!("device power mode {mode:#x}")),
            Err(e) => self.clock.note(format_args!("device power mode: {e}")),
        }
        self.geometry = self
            .read_desc(desc::IDN_GEOMETRY, 0)
            .and_then(|d| GeometryDesc::parse(&d))
            .ok();
        let max_lu = self.geometry.map_or(DEFAULT_MAX_LU, |g| g.max_lu);

        let mut found: Vec<UnitDesc> = Vec::new();
        for lun in 0..max_lu {
            // A unit the device does not know answers with a query error: skip it, as Linux does.
            let raw = match self.read_desc(desc::IDN_UNIT, lun) {
                Ok(raw) => raw,
                Err(e) => {
                    self.clock
                        .note(format_args!("LUN {lun}: unit descriptor: {e}"));
                    continue;
                }
            };
            let unit = match UnitDesc::parse(&raw) {
                Ok(unit) => unit,
                Err(e) => {
                    self.clock.note(format_args!("LUN {lun}: {e}"));
                    continue;
                }
            };
            if unit.enabled != 0 {
                found.push(unit);
            }
        }
        let largest = found
            .iter()
            .filter_map(|u| {
                u.block_size()
                    .map(|bs| u.block_count.saturating_mul(bs as u64))
            })
            .max()
            .unwrap_or(0);

        for unit in found {
            let lun = unit.unit_index;
            let mut role = self.policy.role(lun, &unit, largest);
            self.wait_ready(lun);
            let cap = match self.read_capacity(lun) {
                Ok(c) => c,
                // A unit that will not report its capacity is skipped, not fatal for the others.
                Err(e) => {
                    self.clock.note(format_args!("LUN {lun}: capacity: {e}"));
                    continue;
                }
            };
            // The descriptor and READ CAPACITY must agree before anything is written.
            if role.writable()
                && (Some(cap.block_size) != unit.block_size() || cap.blocks != unit.block_count)
            {
                role = UnitRole::ReadOnly(crate::policy::ReadOnly::BadBlockSize);
            }
            let inquiry = self.inquiry(lun).ok();
            self.units.push(Unit {
                lun,
                desc: unit,
                role,
                block_size: cap.block_size,
                blocks: cap.blocks,
                inquiry,
            });
        }
        Ok(())
    }

    /// TEST UNIT READY until it passes, as `sd_spinup_disk`: each round is one command whose failures are sent again at
    /// once up to 3 times (its `scsi_failure` "allowed = 3"); a round that ends NOT READY is followed by the next 1 s
    /// later (sd's `msleep(1000)` of the spin cycle). sd first sends START STOP UNIT, which the command allowlist
    /// forbids here, so the cycle is ours: [`READY_RETRIES`] rounds.
    fn wait_ready(&mut self, lun: u8) {
        for _ in 0..READY_RETRIES {
            let mut result = self.scsi(lun, &scsi::test_unit_ready(), DataDir::None, &[]);
            for _ in 0..TUR_RETRIES {
                if result.is_ok() || matches!(result, Err(Error::Timeout(_))) {
                    break;
                }
                result = self.scsi(lun, &scsi::test_unit_ready(), DataDir::None, &[]);
            }
            match result {
                Err(Error::Scsi { sense: Some(s), .. }) if s.key == scsi::SENSE_NOT_READY => {
                    self.clock.delay_us(READY_DELAY_US)
                }
                _ => return,
            }
        }
    }

    /// READ CAPACITY(16), falling back to (10). As `sd_read_capacity_16`, UNIT ATTENTION after a reset is sent again at
    /// once, up to `READ_CAPACITY_RETRIES_ON_RESET` (10) times.
    fn read_capacity(&mut self, lun: u8) -> Result<Capacity, Error> {
        let seg = self.bounce(32);
        for _ in 0..=READ_CAPACITY_RETRIES_ON_RESET {
            match self.scsi(
                lun,
                &scsi::read_capacity_16(32),
                DataDir::FromDevice,
                &[seg],
            ) {
                Ok(()) => return Capacity::parse_16(&self.read_bounce(32)),
                Err(Error::Scsi { sense: Some(s), .. }) if s.key == scsi::SENSE_UNIT_ATTENTION => {}
                Err(_) => break,
            }
        }
        let seg = self.bounce(8);
        self.scsi(lun, &scsi::read_capacity_10(), DataDir::FromDevice, &[seg])?;
        Capacity::parse_10(&self.read_bounce(8))
    }

    fn inquiry(&mut self, lun: u8) -> Result<Inquiry, Error> {
        let seg = self.bounce(36);
        self.scsi(lun, &scsi::inquiry(36), DataDir::FromDevice, &[seg])?;
        Inquiry::parse(&self.read_bounce(36))
    }

    /// MODE SENSE (10) of the Caching page of `lun` (`pc`: [`scsi::MODE_PC_CURRENT`] or
    /// [`scsi::MODE_PC_CHANGEABLE`]). Read only, and through the same path as [`Self::scsi`], so the command
    /// allowlist applies. A device answers with no more than it has, so unlike a READ a short transfer is fine here:
    /// only the bytes the residual count says arrived are parsed.
    pub fn mode_sense_caching(&mut self, lun: u8, pc: u8) -> Result<ModeSense, Error> {
        let seg = self.bounce(MODE_SENSE_ALLOC);
        let cdb = scsi::mode_sense_10(scsi::MODE_PAGE_CACHING, pc, MODE_SENSE_ALLOC as u16);
        let residual = self.command(lun, &cdb, DataDir::FromDevice, &[seg], true)?;
        let moved = MODE_SENSE_ALLOC.saturating_sub(residual as usize);
        ModeSense::parse(&self.read_bounce(moved))
    }

    /// The write cache report of `lun` for the boot log (see [`crate::cache`]), `None` if there is no such unit. Read
    /// only; a failed MODE SENSE is in the report, never an error.
    pub fn cache_report(&mut self, lun: u8) -> Option<CacheReport> {
        let unit = self.unit(lun)?.desc;
        let current = self.mode_sense_caching(lun, scsi::MODE_PC_CURRENT);
        let changeable = current
            .is_ok()
            .then(|| self.mode_sense_caching(lun, scsi::MODE_PC_CHANGEABLE));
        Some(CacheReport {
            lun,
            current,
            changeable,
            unit,
            device: self.device,
        })
    }

    /// Reads the GPT of `lun` (header and up to 16 blocks of entries) through the bounce buffer.
    /// Returns `None` for a unit without one.
    pub fn read_gpt(
        &mut self,
        lun: u8,
    ) -> Result<Option<(crate::gpt::Header, Vec<crate::gpt::Entry>)>, Error> {
        let unit = *self.unit(lun).ok_or(Error::NoUnit(lun))?;
        let bs = unit.block_size as usize;
        if bs > BOUNCE_SIZE || unit.blocks < 2 {
            return Ok(None);
        }
        let seg = self.bounce(bs);
        self.read(lun, 1, 1, &[seg])?;
        let Some(header) = crate::gpt::parse_header(&self.read_bounce(bs)) else {
            return Ok(None);
        };
        let bytes = (header.entries as usize).saturating_mul(header.entry_size as usize);
        let blocks = bytes.div_ceil(bs).min(16) as u64;
        let mut array = Vec::with_capacity(blocks as usize * bs);
        for i in 0..blocks {
            let lba = header.entries_lba.saturating_add(i);
            if lba >= unit.blocks {
                break;
            }
            self.read(lun, lba, 1, &[seg])?;
            array.extend_from_slice(&self.read_bounce(bs));
        }
        let entries = crate::gpt::parse_entries(&header, &array);
        Ok(Some((header, entries)))
    }

    // ---- requests ----

    /// Reads a descriptor (query READ DESCRIPTOR). Returns the descriptor bytes.
    pub fn read_desc(&mut self, idn: u8, index: u8) -> Result<Vec<u8>, Error> {
        let rsp = self.query(Query::read_desc(idn, index))?;
        let len = (rsp.data_len as usize).min(UPIU_AREA - UPIU_HEADER_AND_TSF);
        let mut d = alloc::vec![0u8; len];
        self.area
            .read(UCD_OFFSET + RSP_OFFSET + UPIU_HEADER_AND_TSF, &mut d);
        Ok(d)
    }

    /// Reads an attribute (query READ ATTRIBUTE).
    pub fn read_attr(&mut self, idn: u8, index: u8) -> Result<u32, Error> {
        Ok(self.query(Query::read_attr(idn, index))?.value)
    }

    /// Reads (or, during bring-up only, sets) a flag; returns the response and the flag value.
    fn query_flag(&mut self, q: Query) -> Result<(upiu::QueryResponse, u32), Error> {
        let rsp = self.query(q)?;
        Ok((rsp, rsp.value & 1))
    }

    /// A query request, tried `QUERY_REQ_RETRIES` times as Linux does; a query response code that is an error is
    /// returned at once (Linux does not retry `-EINVAL`).
    fn query(&mut self, q: Query) -> Result<upiu::QueryResponse, Error> {
        let mut last = Error::NoDevice;
        for _ in 0..QUERY_RETRIES {
            let r = (|| {
                self.exec(&q.encode(TAG), DataDir::None, &[], QUERY_TIMEOUT_US)?;
                let rsp = upiu::parse_query_response(&self.read_response())?;
                if rsp.response != 0 {
                    return Err(Error::Query(rsp.response));
                }
                Ok(rsp)
            })();
            match r {
                Ok(rsp) => return Ok(rsp),
                Err(e @ Error::Query(_)) => return Err(e),
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    /// Sends a SCSI command to `lun` if the write policy allows it, moving data through `data`.
    pub fn scsi(
        &mut self,
        lun: u8,
        cdb: &[u8],
        dir: DataDir,
        data: &[DataSegment],
    ) -> Result<(), Error> {
        self.command(lun, cdb, dir, data, false).map(|_| ())
    }

    /// [`Self::scsi`], which fails a short transfer unless `short_ok`; with `short_ok` it returns the residual count
    /// (bytes not moved; 0 when the device set the overflow flag, whose count means nothing).
    fn command(
        &mut self,
        lun: u8,
        cdb: &[u8],
        dir: DataDir,
        data: &[DataSegment],
        short_ok: bool,
    ) -> Result<u32, Error> {
        let role = match self.unit(lun) {
            Some(u) => u.role,
            // During the scan the unit is not in the list yet: read-only commands only.
            None => UnitRole::ReadOnly(crate::policy::ReadOnly::NotMain),
        };
        if !WritePolicy::allows(role, lun, cdb) {
            return Err(Error::Refused {
                lun,
                opcode: cdb.first().copied().unwrap_or(0),
            });
        }
        let total: usize = data.iter().map(|s| s.len).sum();
        if (dir == DataDir::None) != (total == 0) {
            return Err(Error::BadBuffer);
        }
        let upiu = upiu::command(lun, TAG, dir, total as u32, cdb);
        let timeout = if cdb.first() == Some(&scsi::SYNCHRONIZE_CACHE_10) {
            FLUSH_TIMEOUT_US
        } else {
            SCSI_TIMEOUT_US
        };
        if let Err(e) = self.exec(&upiu, dir, data, timeout) {
            self.note_request(&e);
            // The device may still hold task tag 0, and there is no task management to abort it.
            if matches!(e, Error::Timeout(_)) {
                self.dead = true;
                self.clock
                    .note(format_args!("a SCSI command timed out: no more requests"));
            }
            return Err(e);
        }
        let rsp = upiu::parse_command_response(&self.read_response())?;
        if rsp.response != 0 {
            return Err(Error::Response(rsp.response));
        }
        if rsp.status != upiu::SCSI_GOOD {
            return Err(Error::Scsi {
                status: rsp.status,
                sense: rsp.sense,
            });
        }
        // Linux sets the residual from the response (ufshcd_transfer_rsp_status) and sd fails a short transfer:
        // a non-zero residual with the overflow flag clear means fewer bytes moved than asked.
        if dir != DataDir::None && rsp.residual != 0 && rsp.flags & upiu::RSP_FLAG_OVERFLOW == 0 {
            if !short_ok {
                return Err(Error::BadData("residual transfer count"));
            }
            return Ok(rsp.residual);
        }
        Ok(0)
    }

    /// Reads `blocks` blocks at `lba` of `lun` into `data`.
    pub fn read(
        &mut self,
        lun: u8,
        lba: u64,
        blocks: u32,
        data: &[DataSegment],
    ) -> Result<(), Error> {
        self.rw(false, lun, lba, blocks, data)
    }

    /// Writes `blocks` blocks at `lba` of `lun` from `data`; refused unless `lun` is the main user LUN.
    pub fn write(
        &mut self,
        lun: u8,
        lba: u64,
        blocks: u32,
        data: &[DataSegment],
    ) -> Result<(), Error> {
        self.rw(true, lun, lba, blocks, data)
    }

    fn rw(
        &mut self,
        write: bool,
        lun: u8,
        lba: u64,
        blocks: u32,
        data: &[DataSegment],
    ) -> Result<(), Error> {
        let unit = *self.unit(lun).ok_or(Error::NoUnit(lun))?;
        let bytes = blocks as u64 * unit.block_size as u64;
        let total: u64 = data.iter().map(|s| s.len as u64).sum();
        if blocks == 0
            || total != bytes
            || lba
                .checked_add(blocks as u64)
                .is_none_or(|end| end > unit.blocks)
        {
            return Err(Error::BadBuffer);
        }
        let (cdb, len) = scsi::rw(write, lba, blocks);
        let dir = if write {
            DataDir::ToDevice
        } else {
            DataDir::FromDevice
        };
        let mut result = self.scsi(lun, &cdb[..len], dir, data);
        for _ in 0..RW_RETRIES {
            if !retryable(&result) {
                break;
            }
            self.clock.delay_us(10_000);
            result = self.scsi(lun, &cdb[..len], dir, data);
        }
        result
    }

    /// SYNCHRONIZE CACHE on the main LUN (a flush bio); refused for read-only units. Retried like a read or write:
    /// Linux `sd_setup_flush_cmnd` gives the flush `SD_MAX_RETRIES` too.
    pub fn flush(&mut self, lun: u8) -> Result<(), Error> {
        let cdb = scsi::synchronize_cache_10();
        let mut result = self.scsi(lun, &cdb, DataDir::None, &[]);
        for _ in 0..RW_RETRIES {
            if !retryable(&result) {
                break;
            }
            self.clock.delay_us(10_000);
            result = self.scsi(lun, &cdb, DataDir::None, &[]);
        }
        result
    }

    /// The driver's bounce buffer as one data segment of `len` bytes (at most [`BOUNCE_SIZE`]).
    pub fn bounce(&self, len: usize) -> DataSegment {
        DataSegment {
            daddr: self.area.daddr() + BOUNCE_OFFSET as u64,
            len: len.min(BOUNCE_SIZE),
        }
    }

    pub fn read_bounce(&self, len: usize) -> Vec<u8> {
        let mut b = alloc::vec![0u8; len.min(BOUNCE_SIZE)];
        self.area.sync_from_device(BOUNCE_OFFSET, b.len());
        self.area.read(BOUNCE_OFFSET, &mut b);
        b
    }

    pub fn write_bounce(&self, data: &[u8]) {
        let n = data.len().min(BOUNCE_SIZE);
        self.area.write(BOUNCE_OFFSET, &data[..n]);
        self.area.sync_to_device(BOUNCE_OFFSET, n);
    }

    fn read_response(&self) -> [u8; UPIU_AREA] {
        let mut r = [0u8; UPIU_AREA];
        self.area.read(UCD_OFFSET + RSP_OFFSET, &mut r);
        r
    }

    /// Puts one request in slot 0, rings the door bell and polls until the controller gives the slot back.
    fn exec(
        &mut self,
        upiu: &[u8; UPIU_HEADER_AND_TSF],
        dir: DataDir,
        data: &[DataSegment],
        timeout_us: u64,
    ) -> Result<(), Error> {
        if self.dead {
            return Err(Error::Dead);
        }
        if data.len() > PRDT_MAX {
            return Err(Error::BadBuffer);
        }
        // Caller memory this request names: the host's own area (the bounce buffer) is never freed anyway.
        let area = self.area.daddr()..self.area.daddr() + AREA_SIZE as u64;
        let caller_bytes: usize = data
            .iter()
            .filter(|s| !area.contains(&s.daddr))
            .map(|s| s.len)
            .sum();
        let mut prdt = Vec::with_capacity(data.len() * upiu::PRDT_ENTRY_SIZE);
        for s in data {
            if !self.probe.cap.addr64 && s.daddr + s.len as u64 > 1 << 32 {
                return Err(Error::BadBuffer);
            }
            prdt.extend_from_slice(&upiu::prdt_entry(s.daddr, s.len)?);
        }
        if self.regs.read32(regs::UTRLDBR) & SLOT0 != 0 {
            return Err(Error::Timeout("slot 0 to be free"));
        }

        let ucd = self.area.daddr() + UCD_OFFSET as u64;
        let utrd = Utrd {
            dir,
            interrupt: true,
            ucd,
            rsp_offset: RSP_OFFSET as u16,
            rsp_len: UPIU_AREA as u16,
            prdt_offset: PRDT_OFFSET as u16,
            prdt_entries: data.len() as u16,
        };
        let mut cmd = [0u8; UPIU_AREA];
        cmd[..UPIU_HEADER_AND_TSF].copy_from_slice(upiu);
        self.area.write(UCD_OFFSET, &cmd);
        self.area.write(UCD_OFFSET + RSP_OFFSET, &[0u8; UPIU_AREA]);
        self.area.write(UCD_OFFSET + PRDT_OFFSET, &prdt);
        self.area.write(UTRL_OFFSET, &utrd.encode());
        self.area.sync_to_device(UTRL_OFFSET, UTRD_SIZE);
        self.area
            .sync_to_device(UCD_OFFSET, PRDT_OFFSET + prdt.len());

        self.regs.write32(regs::IS, regs::IS_UTRCS);
        self.regs.write32(regs::UTRLDBR, SLOT0);

        // Completion is the door bell clear and the descriptor changed. A bell that reads 0 alone proves nothing: right
        // after it is rung the controller may not have taken it yet.
        let start = self.clock.now_us();
        self.timing = Timing {
            first_read: self.regs.read32(regs::UTRLDBR),
            ..Timing::default()
        };
        let mut done = [0u8; UTRD_SIZE];
        let mut spins = 0u32;
        // When the last sleep on the interrupt began, if it ended with no interrupt.
        let mut expired_sleep: Option<u64> = None;
        loop {
            let mut finished = false;
            if self.regs.read32(regs::UTRLDBR) & SLOT0 == 0 {
                let now = self.clock.now_us().wrapping_sub(start);
                self.timing.dbr_clear_us.get_or_insert(now);
                self.area.sync_from_device(UTRL_OFFSET, UTRD_SIZE);
                self.area.read(UTRL_OFFSET, &mut done);
                if upiu::ocs(&done) != upiu::OCS_INVALID_COMMAND_STATUS {
                    self.timing.ocs_us = Some(now);
                    finished = true;
                }
            } else {
                // Rung again later, or still pending: the clear above was not the end of this request.
                self.timing.dbr_clear_us = None;
            }
            if let Some(slept_at) = expired_sleep.take() {
                let now = self.clock.now_us();
                // WaitAgain and TimeOut need nothing here: the checks below sleep again or time the request out, as
                // they did before the sleeps were capped.
                if irq_wait::after_sleep(finished, now.wrapping_sub(start), timeout_us)
                    == irq_wait::AfterSleep::CompleteMissed
                {
                    self.note_missed(now.wrapping_sub(slept_at));
                }
            }
            if finished {
                break;
            }
            let is = self.regs.read32(regs::IS);
            // A UIC error, as Linux `ufshcd_update_uic_error`: read the UEC registers (they clear on read), log
            // them, and clear IS.UE. Only the class Linux treats as needing a reset is fatal (below).
            let ue_fatal = if is & regs::IS_UE != 0 {
                let [pa, dl, n, t, dme] = [
                    regs::UECPA,
                    regs::UECDL,
                    regs::UECN,
                    regs::UECT,
                    regs::UECDME,
                ]
                .map(|r| self.regs.read32(r));
                self.clock.note(format_args!(
                    "UIC error: PA {pa:#x} DL {dl:#x} N {n:#x} T {t:#x} DME {dme:#x}"
                ));
                self.regs.write32(regs::IS, regs::IS_UE);
                // UECDL PA_INIT is the one UIC error class `ufshcd_is_saved_err_fatal` counts as fatal.
                dl & regs::UECDL_PA_INIT_ERROR != 0
            } else {
                false
            };
            if is & regs::IS_FATAL != 0 || ue_fatal {
                // Described before the slot is cleared: afterwards the registers show nothing.
                self.note_request(&Error::Fatal(is));
                self.regs
                    .write32(regs::IS, is & (regs::IS_FATAL | regs::IS_UE));
                self.abort();
                // Linux's error handler resets the host on these bits; this driver cannot reset (no PHY/GCC/RPMh
                // reset), so the host is dead: Linux's "reset failed" outcome.
                self.dead = true;
                self.held = caller_bytes;
                self.clock.note(format_args!(
                    "fatal controller error IS {is:#x}: no more requests"
                ));
                return Err(Error::Fatal(is));
            }
            if self.clock.now_us().wrapping_sub(start) > timeout_us {
                if self.timing.dbr_clear_us.is_some() {
                    // The slot came back and the descriptor did not change: the controller may have run another
                    // descriptor, from a list it still takes to be at the old base. Nothing more goes to it.
                    let e = Error::Ocs(upiu::OCS_INVALID_COMMAND_STATUS);
                    self.note_request(&e);
                    self.dead = true;
                    self.held = caller_bytes;
                    return Err(e);
                }
                self.note_request(&Error::Timeout("a transfer request"));
                self.abort();
                // Cleared or not, the request is not proved over: with the slot still set the controller keeps its
                // PRDT; with it cleared the device may still hold task tag 0 (no ABORT TASK is sent) and send data for
                // it. Linux aborts the task, then frees, and its IOMMU would catch a late access. Adapted: Juno's
                // stream is SMMU bypass, so the buffers are kept for good instead (`scsi` marks the host dead).
                self.held = caller_bytes;
                return Err(Error::Timeout("a transfer request"));
            }
            // Spin briefly (most requests finish in microseconds), then sleep until the controller interrupts, at most
            // 20 ms at a time, so that a request whose interrupt was lost is found soon (`irq_wait`); without an
            // interrupt, back off.
            spins += 1;
            if spins > 64 {
                let at = self.clock.now_us();
                let sleep = irq_wait::sleep_us(at.wrapping_sub(start), timeout_us);
                match self.clock.wait_interrupt(sleep) {
                    IrqWait::Unavailable => {
                        self.clock.delay_us(if spins > 1024 { 1000 } else { 10 })
                    }
                    IrqWait::Interrupted => {}
                    IrqWait::Expired => expired_sleep = Some(at),
                }
            }
        }
        self.regs.write32(regs::IS, regs::IS_UTRCS);
        self.area
            .sync_from_device(UCD_OFFSET + RSP_OFFSET, UPIU_AREA);
        if !self.first_done {
            self.first_done = true;
            self.note_timing("first request: ");
        }
        match upiu::ocs(&done) {
            upiu::OCS_SUCCESS => Ok(()),
            ocs => Err(Error::Ocs(ocs)),
        }
    }

    /// A request finished although its sleep on the interrupt ended with none: counts it, tells the platform, and logs it
    /// (rate-limited; the count is not). `late_us`: since that sleep began, so at most this long after the request finished.
    fn note_missed(&mut self, late_us: u64) {
        let is = self.regs.read32(regs::IS);
        let late_ms = late_us.div_ceil(1000);
        self.missed_irq += 1;
        self.late_ms_max = self.late_ms_max.max(late_ms);
        self.clock.missed_interrupt(late_us);
        let note = self.miss_notes.miss(self.clock.now_us());
        if note.more > 0 {
            self.clock.note(format_args!(
                "missed completion interrupt: {} more not logged",
                note.more
            ));
        }
        if note.line {
            self.clock.note(format_args!(
                "missed completion interrupt: tag {TAG}, {late_ms} ms late, IS={is:#x}"
            ));
        }
    }

    /// Takes slot 0 back after a timeout or fatal error (Linux `ufshcd_clear_cmd`: write 0 to clear a slot).
    fn abort(&mut self) {
        self.regs.write32(regs::UTRLCLR, !SLOT0);
        if self
            .wait_reg(regs::UTRLDBR, SLOT0, 0, CLEAR_TIMEOUT_US, "slot 0 to clear")
            .is_err()
        {
            self.dead = true;
            self.clock
                .note(format_args!("slot 0 will not clear: no more requests"));
        }
    }

    /// Runs a UIC command and returns UCMDARG3 (the attribute value of a DME_GET).
    fn uic(&mut self, cmd: u8, arg1: u32, arg2: u32, arg3: u32) -> Result<u32, Error> {
        self.wait_reg(
            regs::HCS,
            regs::HCS_UCRDY,
            regs::HCS_UCRDY,
            UIC_TIMEOUT_US,
            "UIC command ready",
        )?;
        self.regs.write32(regs::IS, regs::IS_UCCS);
        self.regs.write32(regs::UCMDARG1, arg1);
        self.regs.write32(regs::UCMDARG2, arg2);
        self.regs.write32(regs::UCMDARG3, arg3);
        self.regs.write32(regs::UICCMD, cmd as u32);
        self.wait_reg(
            regs::IS,
            regs::IS_UCCS,
            regs::IS_UCCS,
            UIC_TIMEOUT_US,
            "UIC command completion",
        )?;
        self.regs.write32(regs::IS, regs::IS_UCCS);
        let result = (self.regs.read32(regs::UCMDARG2) & regs::UIC_RESULT_MASK) as u8;
        if result != 0 {
            return Err(Error::Uic { cmd, result });
        }
        Ok(self.regs.read32(regs::UCMDARG3))
    }

    fn wait_reg(
        &self,
        offset: usize,
        mask: u32,
        value: u32,
        timeout_us: u64,
        what: &'static str,
    ) -> Result<(), Error> {
        let start = self.clock.now_us();
        loop {
            if self.regs.read32(offset) & mask == value {
                return Ok(());
            }
            if self.clock.now_us().wrapping_sub(start) > timeout_us {
                return Err(Error::Timeout(what));
            }
            self.clock.delay_us(100);
        }
    }
}

/// Whether a failed request is sent again, as Linux takes these: UNIT ATTENTION, BUSY, TASK SET FULL and TASK
/// ABORTED (0x40) statuses from the SCSI layer, and the OCS values `ufshcd_transfer_rsp_status` requeues
/// (OCS_ABORTED, OCS_INVALID_COMMAND_STATUS while the slot is still ours).
fn retryable(result: &Result<(), Error>) -> bool {
    match result {
        Err(Error::Scsi { status, sense }) => {
            *status == upiu::SCSI_BUSY
                || *status == upiu::SCSI_TASK_SET_FULL
                || *status == upiu::SCSI_TASK_ABORTED
                || sense.is_some_and(|s| s.key == scsi::SENSE_UNIT_ATTENTION)
        }
        Err(Error::Ocs(upiu::OCS_ABORTED)) | Err(Error::Ocs(upiu::OCS_INVALID_COMMAND_STATUS)) => {
            true
        }
        _ => false,
    }
}

/// [`Host::quiesce`] for when the host itself cannot be reached, because a CPU stopped in the middle of a request holds its
/// lock. It gives a request in flight a moment, by polling, and stops the lists regardless.
pub fn stop_lists_of(regs: &impl Regs) {
    const POLLS: usize = 200_000;
    for _ in 0..POLLS {
        if regs.read32(regs::UTRLDBR) & SLOT0 == 0 {
            break;
        }
        core::hint::spin_loop();
    }
    regs.write32(regs::UTRLRSR, 0);
    regs.write32(regs::UTMRLRSR, 0);
}
