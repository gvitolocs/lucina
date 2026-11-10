// SPDX-License-Identifier: ISC
//
// Derived from Linux drivers/net/wireless/ath/ath10k/qmi.c (ISC licence, Copyright (c) 2018 The Linux
// Foundation, Copyright (c) 2022-2023 Qualcomm Innovation Center, Inc.): the order of the requests, what each
// response is checked for, and which failures are fatal.

//! The WLFW client of `ath10k/qmi.c` as a state machine with no I/O and no timers.
//!
//! The owner feeds it [`Input`]s (the service appeared, a QMI message arrived, the answer to a question the
//! machine asked, a timer fired) and gets back [`Output`]s: messages to send, questions to answer, and
//! [facts](Fact) learned. It follows, step by step:
//!
//! * `ath10k_qmi_event_server_arrive()` (the work item that runs when the WLFW server shows up): indication
//!   register; if the response says the firmware is already ready, stop there; host capabilities (unless
//!   `ATH10K_SNOC_FLAG_SKIP_HOST_CAP_QUIRK`); MSA info and the range check of its regions; the permission
//!   assignment (skipped with `qcom,msa-fixed-perm`, which is [`Config::msa_fixed_perm`]); MSA ready; capabilities.
//! * `ath10k_qmi_event_msa_ready()` (runs on the MSA_READY indication, or straight after the first work item
//!   with `qcom,no-msa-ready-indicator`): the board data file is fetched by the owner
//!   ([`Output::NeedBoardData`]), cut into [`MAX_DATA_SIZE`](crate::wlfw::MAX_DATA_SIZE) chunks and sent one
//!   request at a time; then the calibration report.
//! * `ath10k_qmi_event_fw_ready_ind()`: the FW_READY indication marks the firmware ready.
//! * `ath10k_qmi_wlan_enable()`: the CE configuration, then the driver mode. Also `ath10k_qmi_wlan_disable()`
//!   (mode off) and `ath10k_qmi_set_fw_log_mode()`.
//!
//! Indications that arrive while one of the two work items is running are held and handled when it ends, like
//! the C, where they are queued on the same ordered workqueue.
//!
//! Where it differs from the C, on purpose:
//!
//! * With `qcom,no-msa-ready-indicator` the C runs the MSA-ready item after the server-arrive item even when
//!   that failed or stopped early because the firmware was already ready. Here it only follows a complete one.
//! * A response is matched on its transaction id and its message id; the C looks at the transaction id only.
//! * The 20 ms pause the C takes between the MSA info response and the permission assignment is an advisory
//!   [`Output::Pause`]; nothing here waits.
//!
//! Behaviour copied although it looks wrong, so that the machine and the C agree: a rejected MSA-ready response
//! is only logged (`ath10k_qmi_msa_ready_send_sync_msg()` sets an error and then returns 0); on the last board
//! data chunk any failure result is tolerated (the C compares the response's `result`, 0 or 1, with
//! `QMI_ERR_MALFORMED_MSG_V01`, which is 1, instead of comparing `error`); and the calibration report's outcome
//! is ignored.

use crate::codec::{
    ERR_MALFORMED_MSG, ERR_NOT_SUPPORTED, Error, Header, INDICATION, Message, REQUEST, RESPONSE,
    decode_message, encode_message,
};
use crate::wlfw::{
    self, BdfDownloadReq, BdfDownloadResp, CalReportReq, CalReportResp, CapReq, CapResp,
    CeSvcPipeCfg, CeTgtPipeCfg, FwReadyInd, HostCapReq, HostCapReq8bit, HostCapResp,
    IndRegisterReq, IndRegisterResp, IniReq, IniResp, MAX_DATA_SIZE, MAX_NUM_CE,
    MAX_NUM_SHADOW_REG, MAX_NUM_SVC, MsaInfoReq, MsaInfoResp, MsaReadyInd, MsaReadyReq,
    MsaReadyResp, ShadowRegCfg, WlanCfgReq, WlanCfgResp, WlanModeReq, WlanModeResp, driver_mode,
    fw_status, msg_id,
};
use alloc::collections::VecDeque;
use alloc::vec::Vec;

/// `ATH10K_QMI_TIMEOUT`: seconds the C waits for any response. The owner runs the timer and feeds
/// [`Input::Timeout`].
pub const TXN_TIMEOUT_SECS: u32 = 30;
/// The `msleep(20)` between the MSA info response and the permission assignment.
pub const MSA_SETTLE_MS: u32 = 20;
/// What `ath10k_qmi_cap_send_sync_msg()` stores when the firmware does not give a chip or board id.
pub const ID_UNKNOWN: u32 = 0xFF;

/// What the owner knows about the machine and the board, from the device tree and ath10k's quirk flags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// `ar->msa.paddr`: the memory the host sets aside for the firmware's MSA (here 0x93900000).
    pub msa_phys: u64,
    /// `ar->msa.mem_size` (here 0x200000).
    pub msa_size: u32,
    /// `qcom,msa-fixed-perm`: the permissions are already right, so no assignment step.
    pub msa_fixed_perm: bool,
    /// `qcom,no-msa-ready-indicator`: the firmware never sends MSA_READY; go on without it.
    pub no_msa_ready_indicator: bool,
    /// `ATH10K_SNOC_FLAG_SKIP_HOST_CAP_QUIRK`.
    pub skip_host_cap: bool,
    /// `ATH10K_SNOC_FLAG_8BIT_HOST_CAP_QUIRK`.
    pub host_cap_8bit: bool,
    /// `ar_snoc->xo_cal_supported`, with the value `ar_snoc->xo_cal_data`.
    pub xo_cal_supported: bool,
    pub xo_cal_data: u8,
}

impl Config {
    /// All flags off.
    pub fn new(msa_phys: u64, msa_size: u32) -> Config {
        Config {
            msa_phys,
            msa_size,
            msa_fixed_perm: false,
            no_msa_ready_indicator: false,
            skip_host_cap: false,
            host_cap_8bit: false,
            xo_cal_supported: false,
            xo_cal_data: 0,
        }
    }
}

/// `struct ath10k_msa_mem_info`: one region the firmware asked for inside the MSA.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MsaRegion {
    pub addr: u64,
    pub size: u32,
    pub secure: u8,
}

/// What the capability response told. The ids are [`ID_UNKNOWN`] until the firmware says otherwise, and the
/// others keep their last value when a later response leaves them out, as the C's `qmi->*` fields do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capabilities {
    pub chip_id: u32,
    pub chip_family: u32,
    pub board_id: u32,
    pub soc_id: u32,
    pub fw_version: u32,
    pub fw_build_timestamp: Vec<u8>,
    pub fw_build_id: Vec<u8>,
    /// Not kept by the C.
    pub num_macs: Option<u8>,
}

impl Default for Capabilities {
    fn default() -> Self {
        Capabilities {
            chip_id: ID_UNKNOWN,
            chip_family: 0,
            board_id: ID_UNKNOWN,
            soc_id: 0,
            fw_version: 0,
            fw_build_timestamp: Vec::new(),
            fw_build_id: Vec::new(),
            num_macs: None,
        }
    }
}

/// `struct ath10k_qmi_wlan_enable_cfg`: the copy-engine tables of the host driver. Longer tables than the
/// message holds are cut to its maximum, as `ath10k_qmi_cfg_send_sync_msg()` does.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WlanEnableCfg {
    pub ce_tgt_cfg: Vec<CeTgtPipeCfg>,
    pub ce_svc_cfg: Vec<CeSvcPipeCfg>,
    pub shadow_reg_cfg: Vec<ShadowRegCfg>,
}

/// The requests the machine sends; names the operation in a [`Failure`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    IndRegister,
    HostCap,
    MsaInfo,
    MsaReady,
    Cap,
    BdfDownload,
    CalReport,
    WlanCfg,
    WlanMode,
    FwLogMode,
}

/// Why an operation stopped. After the first four the machine is [`State::Failed`] until the next
/// [`Input::ServerArrive`]; a failed enable, disable or log-mode request leaves the firmware ready.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// The firmware answered with a failure result; `error` is its QMI error code.
    Rejected { step: Step, error: u16 },
    /// The response could not be decoded.
    Decode { step: Step, error: Error },
    /// [`Input::Timeout`] while this request was out.
    Timeout { step: Step },
    /// A region in the MSA info response lies outside the memory the host offered
    /// (`ath10k_qmi_msa_mem_info_send_sync_msg()`).
    MsaRegionOutOfRange { addr: u64, size: u32 },
    /// The owner could not assign the MSA permissions.
    MsaPermissions,
    /// The owner has no board data file for this chip and board.
    BoardData,
    /// A request would not encode (a table longer than the message allows, for instance).
    Encode(Error),
    /// A request was asked for while another is still out.
    Busy,
    /// WLAN enable asked before the firmware said it is ready.
    NotReady,
}

/// Where the machine is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// No WLFW server.
    Idle,
    /// A request is out and its response is awaited.
    Awaiting(Step),
    /// Waiting for [`Input::MsaPermissions`].
    AwaitingMsaPermissions,
    /// The server-arrive work is done; waiting for the MSA_READY indication.
    AwaitingMsaReady,
    /// Waiting for [`Input::BoardData`].
    AwaitingBoardData,
    /// The board data and calibration report are done; waiting for the FW_READY indication.
    AwaitingFwReady,
    /// The firmware is ready for [`Input::WlanEnable`].
    Ready,
    /// A step failed; see the [`Failure`] that was reported.
    Failed,
}

/// Something that happened to or in the machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input<'a> {
    /// The WLFW service appeared in the name service (`ath10k_qmi_new_server()`). Starts over from the top.
    ServerArrive,
    /// The service went away (`ath10k_qmi_del_server()`).
    ServerExit,
    /// A QMI message from the service: the bytes of one QRTR packet's payload.
    Message(&'a [u8]),
    /// The answer to [`Output::AssignMsa`]: whether `qcom_scm_assign_mem()` worked for every region.
    MsaPermissions(bool),
    /// The answer to [`Output::NeedBoardData`]: the file contents.
    BoardData(Vec<u8>),
    /// The answer to [`Output::NeedBoardData`] when there is no file (`ath10k_core_fetch_board_file()` failed).
    BoardDataUnavailable,
    /// [`TXN_TIMEOUT_SECS`] passed with a request out.
    Timeout,
    /// `ath10k_qmi_wlan_enable()`: configuration, then mode (`driver_mode`).
    WlanEnable { cfg: WlanEnableCfg, mode: i32 },
    /// `ath10k_qmi_wlan_disable()`.
    WlanDisable,
    /// `ath10k_qmi_set_fw_log_mode()`.
    SetFwLogMode(u8),
}

/// What the machine wants done or reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Output {
    /// Send this QMI message (header included) to the WLFW service's QRTR port.
    Send(Vec<u8>),
    /// The C sleeps this long here ([`MSA_SETTLE_MS`]) before it touches the MSA permissions. Advisory.
    Pause {
        ms: u32,
    },
    /// Give the firmware's VMIDs access to these regions (`ath10k_qmi_map_msa_permission()`: HLOS to MSS_MSA
    /// and WLAN, plus WLAN_CE unless `secure`), then answer with [`Input::MsaPermissions`].
    AssignMsa(Vec<MsaRegion>),
    /// Hand the regions back to HLOS (`ath10k_qmi_unmap_msa_permission()`). No answer needed.
    UnassignMsa(Vec<MsaRegion>),
    /// Find the board data file for this board and chip, then answer with [`Input::BoardData`] or
    /// [`Input::BoardDataUnavailable`]. The ids are [`ID_UNKNOWN`] when the firmware gave none.
    NeedBoardData {
        board_id: u32,
        chip_id: u32,
    },
    Fact(Fact),
    Failed(Failure),
}

/// Something learned, for logs and for the driver above.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fact {
    /// The indication-register response; `fw_ready` when its `fw_status` has FW_READY.
    IndicationsRegistered {
        fw_ready: bool,
    },
    /// The host capability response (`unsupported`: older firmware answered NOT_SUPPORTED, which is fine).
    HostCapDone {
        unsupported: bool,
    },
    /// The regions the firmware asked for, after the range check.
    MsaRegions(Vec<MsaRegion>),
    /// The MSA-ready response; `accepted` false means the firmware refused, which the C only logs.
    MsaReadyAcked {
        accepted: bool,
    },
    Capabilities(Capabilities),
    /// The MSA_READY indication arrived and the board data step begins.
    MsaReadyIndication,
    /// MSA_READY arrived although the firmware is not meant to send it (`qcom,no-msa-ready-indicator`).
    UnexpectedMsaReady,
    /// All board data chunks are sent. `crc_rejected`: the last chunk's response was a failure that the C
    /// treats as non-fatal.
    BdfDownloaded {
        chunks: u32,
        crc_rejected: bool,
    },
    /// The calibration report is answered or given up on. Nothing depends on it.
    CalReported {
        accepted: bool,
    },
    /// The firmware is ready (FW_READY indication, or the registration response said so).
    FwReady,
    WlanConfigured,
    /// The mode request succeeded.
    ModeSet {
        mode: i32,
    },
    FwLogModeSet {
        mode: u8,
    },
    /// [`Input::ServerExit`] handled.
    ServerGone,
}

#[derive(Debug, Clone, Copy)]
struct Pending {
    txn: u16,
    step: Step,
}

#[derive(Debug)]
struct BdfJob {
    data: Vec<u8>,
    offset: usize,
    seg_id: u32,
    chunks: u32,
    /// Whether the chunk in flight is the last one (`req->end`).
    last: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ind {
    FwReady,
    MsaReady,
}

/// The ath10k WLFW client. See the [module documentation](self).
#[derive(Debug)]
pub struct WlfwClient {
    cfg: Config,
    state: State,
    caps: Capabilities,
    regions: Vec<MsaRegion>,
    permissions_assigned: bool,
    fw_ready: bool,
    next_txn: u16,
    pending: Option<Pending>,
    /// One of the two work items of `ath10k_qmi_driver_event_work()` is running.
    work: bool,
    deferred: VecDeque<Ind>,
    bdf: Option<BdfJob>,
    /// The mode `Input::WlanEnable` asked for, between its two requests.
    enable_mode: Option<i32>,
    /// The level `Input::SetFwLogMode` asked for, while its request is out.
    log_mode: Option<u8>,
}

impl WlfwClient {
    pub fn new(cfg: Config) -> WlfwClient {
        WlfwClient {
            cfg,
            state: State::Idle,
            caps: Capabilities::default(),
            regions: Vec::new(),
            permissions_assigned: false,
            fw_ready: false,
            next_txn: 0,
            pending: None,
            work: false,
            deferred: VecDeque::new(),
            bdf: None,
            enable_mode: None,
            log_mode: None,
        }
    }

    pub fn state(&self) -> State {
        self.state
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    /// `qmi->fw_ready`.
    pub fn is_fw_ready(&self) -> bool {
        self.fw_ready
    }

    pub fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    /// The regions of the last accepted MSA info response.
    pub fn msa_regions(&self) -> &[MsaRegion] {
        &self.regions
    }

    /// Feed one input; returns what to do and what was learned, in order.
    pub fn handle(&mut self, input: Input<'_>) -> Vec<Output> {
        let mut out = Vec::new();
        match input {
            Input::ServerArrive => self.server_arrive(&mut out),
            Input::ServerExit => self.server_exit(&mut out),
            Input::Message(buf) => self.message(buf, &mut out),
            Input::MsaPermissions(ok) => self.msa_permissions(ok, &mut out),
            Input::BoardData(data) => self.board_data(data, &mut out),
            Input::BoardDataUnavailable => {
                if self.state == State::AwaitingBoardData {
                    self.fail(Failure::BoardData, &mut out);
                }
            }
            Input::Timeout => {
                if let Some(p) = self.pending.take() {
                    self.step_failed(p.step, Failure::Timeout { step: p.step }, &mut out);
                }
            }
            Input::WlanEnable { cfg, mode } => self.wlan_enable(&cfg, mode, &mut out),
            Input::WlanDisable => self.wlan_mode(driver_mode::OFF, &mut out),
            Input::SetFwLogMode(mode) => {
                if self.busy_or_not_ready(&mut out) {
                    return out;
                }
                let req = IniReq {
                    enablefwlog: Some(mode),
                };
                self.log_mode = Some(mode);
                self.send(Step::FwLogMode, &req, &mut out);
            }
        }
        out
    }

    fn alloc_txn(&mut self) -> u16 {
        // idr_alloc_cyclic(&qmi->txns, txn, 0, U16_MAX): ids 0 to 65534, then round again.
        let id = self.next_txn;
        self.next_txn = if id >= u16::MAX - 1 { 0 } else { id + 1 };
        id
    }

    /// Encode and queue a request, remembering which response to expect.
    fn send<M: Message>(&mut self, step: Step, req: &M, out: &mut Vec<Output>) -> bool {
        let txn = self.alloc_txn();
        match encode_message(REQUEST, txn, req) {
            Ok(bytes) => {
                self.pending = Some(Pending { txn, step });
                self.state = State::Awaiting(step);
                out.push(Output::Send(bytes));
                true
            }
            Err(e) => {
                self.step_failed(step, Failure::Encode(e), out);
                false
            }
        }
    }

    fn fail(&mut self, f: Failure, out: &mut Vec<Output>) {
        self.state = State::Failed;
        self.pending = None;
        self.work = false;
        self.bdf = None;
        self.enable_mode = None;
        self.deferred.clear();
        out.push(Output::Failed(f));
    }

    /// A request ended without a usable answer (rejected, undecodable, timed out). What that means depends on
    /// the step.
    fn step_failed(&mut self, step: Step, f: Failure, out: &mut Vec<Output>) {
        self.pending = None;
        match step {
            // ath10k_qmi_msa_ready_send_sync_msg() and ath10k_qmi_cap_send_sync_msg() failing sends the
            // arrive work to err_setup_msa, which hands the regions back.
            Step::MsaReady | Step::Cap => {
                self.unassign(out);
                self.fail(f, out);
            }
            Step::IndRegister | Step::HostCap | Step::MsaInfo | Step::BdfDownload => {
                self.fail(f, out)
            }
            // ath10k_qmi_event_msa_ready() ignores the report's result.
            Step::CalReport => self.cal_report_done(false, out),
            Step::WlanCfg | Step::WlanMode | Step::FwLogMode => {
                self.enable_mode = None;
                self.log_mode = None;
                self.state = self.ready_state();
                out.push(Output::Failed(f));
            }
        }
    }

    fn ready_state(&self) -> State {
        if self.fw_ready {
            State::Ready
        } else {
            State::Idle
        }
    }

    fn unassign(&mut self, out: &mut Vec<Output>) {
        if self.permissions_assigned && !self.cfg.msa_fixed_perm {
            out.push(Output::UnassignMsa(self.regions.clone()));
        }
        self.permissions_assigned = false;
    }

    // ---- ath10k_qmi_event_server_arrive() ----

    fn server_arrive(&mut self, out: &mut Vec<Output>) {
        self.pending = None;
        self.bdf = None;
        self.enable_mode = None;
        self.deferred.clear();
        self.work = true;
        let req = IndRegisterReq {
            client_id: Some(wlfw::ATH10K_CLIENT_ID),
            fw_ready_enable: Some(1),
            msa_ready_enable: Some(1),
            xo_cal_enable: self.cfg.xo_cal_supported.then_some(1),
            ..IndRegisterReq::default()
        };
        self.send(Step::IndRegister, &req, out);
    }

    fn server_exit(&mut self, out: &mut Vec<Output>) {
        // ath10k_qmi_del_server() clears fw_ready at once; ath10k_qmi_event_server_exit() does the rest.
        self.fw_ready = false;
        self.pending = None;
        self.work = false;
        self.bdf = None;
        self.enable_mode = None;
        self.deferred.clear();
        self.unassign(out);
        self.state = State::Idle;
        out.push(Output::Fact(Fact::ServerGone));
    }

    fn message(&mut self, buf: &[u8], out: &mut Vec<Output>) {
        let Ok((header, _)) = Header::parse(buf) else {
            return;
        };
        match header.msg_type {
            INDICATION => match header.msg_id {
                // An indication that does not decode is dropped ("failed to decode incoming message").
                msg_id::FW_READY_IND if decode_message::<FwReadyInd>(buf).is_ok() => {
                    self.indication(Ind::FwReady, out);
                }
                msg_id::MSA_READY_IND if decode_message::<MsaReadyInd>(buf).is_ok() => {
                    self.indication(Ind::MsaReady, out);
                }
                _ => {}
            },
            RESPONSE => {
                // "Ignore unexpected responses" (qmi_handle_message()).
                let Some(p) = self.pending else { return };
                if header.txn_id != p.txn || header.msg_id != step_msg_id(p.step) {
                    return;
                }
                self.pending = None;
                self.response(p.step, buf, out);
            }
            _ => {}
        }
    }

    fn response(&mut self, step: Step, buf: &[u8], out: &mut Vec<Output>) {
        match step {
            Step::IndRegister => self.on_ind_register(buf, out),
            Step::HostCap => self.on_host_cap(buf, out),
            Step::MsaInfo => self.on_msa_info(buf, out),
            Step::MsaReady => self.on_msa_ready(buf, out),
            Step::Cap => self.on_cap(buf, out),
            Step::BdfDownload => self.on_bdf(buf, out),
            Step::CalReport => self.on_cal_report(buf, out),
            Step::WlanCfg => self.on_wlan_cfg(buf, out),
            Step::WlanMode => self.on_wlan_mode(buf, out),
            Step::FwLogMode => self.on_fw_log_mode(buf, out),
        }
    }

    /// Decode a response, or report why not. `None` means the failure was already handled.
    fn decode<M: Message>(&mut self, step: Step, buf: &[u8], out: &mut Vec<Output>) -> Option<M> {
        match decode_message::<M>(buf) {
            Ok((_, m)) => Some(m),
            Err(error) => {
                self.step_failed(step, Failure::Decode { step, error }, out);
                None
            }
        }
    }

    fn rejected(&mut self, step: Step, error: u16, out: &mut Vec<Output>) {
        self.step_failed(step, Failure::Rejected { step, error }, out);
    }

    fn on_ind_register(&mut self, buf: &[u8], out: &mut Vec<Output>) {
        let Some(r) = self.decode::<IndRegisterResp>(Step::IndRegister, buf, out) else {
            return;
        };
        if !r.resp.is_success() {
            return self.rejected(Step::IndRegister, r.resp.error, out);
        }
        let ready = r.fw_status.is_some_and(|s| s & fw_status::FW_READY != 0);
        out.push(Output::Fact(Fact::IndicationsRegistered {
            fw_ready: ready,
        }));
        if ready {
            // "if (qmi->fw_ready) { ath10k_snoc_fw_indication(ar, READY); return; }"
            self.fw_ready = true;
            self.state = State::Ready;
            out.push(Output::Fact(Fact::FwReady));
            self.work_done(out);
            return;
        }
        if self.cfg.skip_host_cap {
            self.msa_info(out);
        } else if self.cfg.host_cap_8bit {
            let req = HostCapReq8bit {
                daemon_support: Some(0),
            };
            self.send(Step::HostCap, &req, out);
        } else {
            let req = HostCapReq {
                daemon_support: Some(0),
                ..HostCapReq::default()
            };
            self.send(Step::HostCap, &req, out);
        }
    }

    fn on_host_cap(&mut self, buf: &[u8], out: &mut Vec<Output>) {
        let Some(r) = self.decode::<HostCapResp>(Step::HostCap, buf, out) else {
            return;
        };
        // Older firmware does not know the request; that is not fatal.
        if !r.resp.is_success() && r.resp.error != ERR_NOT_SUPPORTED {
            return self.rejected(Step::HostCap, r.resp.error, out);
        }
        out.push(Output::Fact(Fact::HostCapDone {
            unsupported: !r.resp.is_success(),
        }));
        self.msa_info(out);
    }

    fn msa_info(&mut self, out: &mut Vec<Output>) {
        let req = MsaInfoReq {
            msa_addr: self.cfg.msa_phys,
            size: self.cfg.msa_size,
        };
        self.send(Step::MsaInfo, &req, out);
    }

    fn on_msa_info(&mut self, buf: &[u8], out: &mut Vec<Output>) {
        let Some(r) = self.decode::<MsaInfoResp>(Step::MsaInfo, buf, out) else {
            return;
        };
        if !r.resp.is_success() {
            return self.rejected(Step::MsaInfo, r.resp.error, out);
        }
        // The decoder already refused more than MAX_MEM_REG regions (the C's own length check is the second
        // line of defence). Exact arithmetic: the C adds a u32 to a u64 and relies on the first test.
        let base = u128::from(self.cfg.msa_phys);
        let max = base + u128::from(self.cfg.msa_size);
        for m in &r.mem_region_info {
            let addr = u128::from(m.region_addr);
            if m.size > self.cfg.msa_size
                || addr > max
                || addr < base
                || addr + u128::from(m.size) > max
            {
                let f = Failure::MsaRegionOutOfRange {
                    addr: m.region_addr,
                    size: m.size,
                };
                return self.fail(f, out);
            }
        }
        self.regions = r
            .mem_region_info
            .iter()
            .map(|m| MsaRegion {
                addr: m.region_addr,
                size: m.size,
                secure: m.secure_flag,
            })
            .collect();
        out.push(Output::Fact(Fact::MsaRegions(self.regions.clone())));
        out.push(Output::Pause { ms: MSA_SETTLE_MS });
        if self.cfg.msa_fixed_perm || self.regions.is_empty() {
            self.msa_ready(out);
        } else {
            self.state = State::AwaitingMsaPermissions;
            out.push(Output::AssignMsa(self.regions.clone()));
        }
    }

    fn msa_permissions(&mut self, ok: bool, out: &mut Vec<Output>) {
        if self.state != State::AwaitingMsaPermissions {
            return;
        }
        if !ok {
            // ath10k_qmi_setup_msa_permissions() unwinds the regions it had mapped itself.
            return self.fail(Failure::MsaPermissions, out);
        }
        self.permissions_assigned = true;
        self.msa_ready(out);
    }

    fn msa_ready(&mut self, out: &mut Vec<Output>) {
        self.send(Step::MsaReady, &MsaReadyReq, out);
    }

    fn on_msa_ready(&mut self, buf: &[u8], out: &mut Vec<Output>) {
        let Some(r) = self.decode::<MsaReadyResp>(Step::MsaReady, buf, out) else {
            return;
        };
        // ath10k_qmi_msa_ready_send_sync_msg(): a rejection sets ret = -EINVAL, then "return 0".
        out.push(Output::Fact(Fact::MsaReadyAcked {
            accepted: r.resp.is_success(),
        }));
        self.send(Step::Cap, &CapReq, out);
    }

    fn on_cap(&mut self, buf: &[u8], out: &mut Vec<Output>) {
        let Some(r) = self.decode::<CapResp>(Step::Cap, buf, out) else {
            return;
        };
        if !r.resp.is_success() {
            return self.rejected(Step::Cap, r.resp.error, out);
        }
        match r.chip_info {
            Some(c) => {
                self.caps.chip_id = c.chip_id;
                self.caps.chip_family = c.chip_family;
            }
            None => self.caps.chip_id = ID_UNKNOWN,
        }
        self.caps.board_id = r.board_info.map_or(ID_UNKNOWN, |b| b.board_id);
        if let Some(s) = r.soc_info {
            self.caps.soc_id = s.soc_id;
        }
        if let Some(v) = r.fw_version_info {
            self.caps.fw_version = v.fw_version;
            self.caps.fw_build_timestamp = v.fw_build_timestamp;
        }
        if let Some(id) = r.fw_build_id {
            self.caps.fw_build_id = id;
        }
        self.caps.num_macs = r.num_macs;
        out.push(Output::Fact(Fact::Capabilities(self.caps.clone())));
        // The server-arrive work item is complete.
        self.state = State::AwaitingMsaReady;
        self.work_done(out);
        if self.cfg.no_msa_ready_indicator && !self.work {
            self.start_board_data(out);
        }
    }

    // ---- indications ----

    fn indication(&mut self, ind: Ind, out: &mut Vec<Output>) {
        if self.work {
            self.deferred.push_back(ind);
        } else {
            self.process_indication(ind, out);
        }
    }

    fn process_indication(&mut self, ind: Ind, out: &mut Vec<Output>) {
        match ind {
            Ind::FwReady => {
                // ath10k_qmi_event_fw_ready_ind()
                self.fw_ready = true;
                if matches!(self.state, State::Idle | State::Failed) {
                    return;
                }
                if !matches!(self.state, State::Awaiting(_)) {
                    self.state = State::Ready;
                }
                out.push(Output::Fact(Fact::FwReady));
            }
            Ind::MsaReady => {
                if matches!(self.state, State::Idle | State::Failed) {
                    return;
                }
                if self.cfg.no_msa_ready_indicator {
                    out.push(Output::Fact(Fact::UnexpectedMsaReady));
                    return;
                }
                out.push(Output::Fact(Fact::MsaReadyIndication));
                self.start_board_data(out);
            }
        }
    }

    /// A work item ended: run what was held back, in arrival order, until one starts a new item.
    fn work_done(&mut self, out: &mut Vec<Output>) {
        self.work = false;
        while !self.work {
            let Some(ind) = self.deferred.pop_front() else {
                break;
            };
            self.process_indication(ind, out);
        }
    }

    // ---- ath10k_qmi_event_msa_ready() ----

    fn start_board_data(&mut self, out: &mut Vec<Output>) {
        self.work = true;
        self.state = State::AwaitingBoardData;
        out.push(Output::NeedBoardData {
            board_id: self.caps.board_id,
            chip_id: self.caps.chip_id,
        });
    }

    fn board_data(&mut self, data: Vec<u8>, out: &mut Vec<Output>) {
        if self.state != State::AwaitingBoardData {
            return;
        }
        self.bdf = Some(BdfJob {
            data,
            offset: 0,
            seg_id: 0,
            chunks: 0,
            last: false,
        });
        self.next_bdf_chunk(out);
    }

    /// `while (remaining)` of `ath10k_qmi_bdf_dnld_send_sync()`: the next chunk, or on to the calibration report.
    fn next_bdf_chunk(&mut self, out: &mut Vec<Output>) {
        let Some(job) = self.bdf.as_mut() else { return };
        let remaining = job.data.len() - job.offset;
        if remaining == 0 {
            let (chunks, crc_rejected) = (job.chunks, false);
            self.bdf = None;
            out.push(Output::Fact(Fact::BdfDownloaded {
                chunks,
                crc_rejected,
            }));
            return self.cal_report(out);
        }
        // "if (remaining > QMI_WLFW_MAX_DATA_SIZE_V01) {...} else { data_len = remaining; end = 1 }"
        let len = remaining.min(MAX_DATA_SIZE);
        job.last = remaining <= MAX_DATA_SIZE;
        let req = BdfDownloadReq {
            valid: 1,
            file_id: Some(0),
            total_size: Some(job.data.len() as u32),
            seg_id: Some(job.seg_id),
            data: Some(job.data[job.offset..job.offset + len].to_vec()),
            end: Some(u8::from(job.last)),
            bdf_type: None,
        };
        self.send(Step::BdfDownload, &req, out);
    }

    fn on_bdf(&mut self, buf: &[u8], out: &mut Vec<Output>) {
        let Some(r) = self.decode::<BdfDownloadResp>(Step::BdfDownload, buf, out) else {
            return;
        };
        let Some(job) = self.bdf.as_mut() else { return };
        // end = 1 triggers a CRC check on the file; the C lets a failure there through. It compares the
        // response's `result` with QMI_ERR_MALFORMED_MSG_V01 (1), which is the value of QMI_RESULT_FAILURE_V01,
        // so on the last chunk every failure passes, whatever its error code.
        let crc_rejected = !r.resp.is_success() && job.last && r.resp.result == ERR_MALFORMED_MSG;
        if !r.resp.is_success() && !crc_rejected {
            return self.rejected(Step::BdfDownload, r.resp.error, out);
        }
        let len = (job.data.len() - job.offset).min(MAX_DATA_SIZE);
        job.offset += len;
        job.seg_id += 1;
        job.chunks += 1;
        if job.offset == job.data.len() {
            let chunks = job.chunks;
            self.bdf = None;
            out.push(Output::Fact(Fact::BdfDownloaded {
                chunks,
                crc_rejected,
            }));
            self.cal_report(out);
        } else {
            self.next_bdf_chunk(out);
        }
    }

    fn cal_report(&mut self, out: &mut Vec<Output>) {
        // No calibration data is kept in this ath10k (`qmi->cal_data[]` is never filled), so the list is empty.
        let req = CalReportReq {
            meta_data: Vec::new(),
            xo_cal_data: self.cfg.xo_cal_supported.then_some(self.cfg.xo_cal_data),
        };
        self.send(Step::CalReport, &req, out);
    }

    fn on_cal_report(&mut self, buf: &[u8], out: &mut Vec<Output>) {
        let accepted = match decode_message::<CalReportResp>(buf) {
            Ok((_, r)) => r.resp.is_success(),
            Err(_) => false,
        };
        self.cal_report_done(accepted, out);
    }

    fn cal_report_done(&mut self, accepted: bool, out: &mut Vec<Output>) {
        out.push(Output::Fact(Fact::CalReported { accepted }));
        self.state = if self.fw_ready {
            State::Ready
        } else {
            State::AwaitingFwReady
        };
        self.work_done(out);
    }

    // ---- ath10k_qmi_wlan_enable() and friends ----

    fn busy_or_not_ready(&mut self, out: &mut Vec<Output>) -> bool {
        if self.pending.is_some() {
            out.push(Output::Failed(Failure::Busy));
            return true;
        }
        if !self.fw_ready || self.state != State::Ready {
            out.push(Output::Failed(Failure::NotReady));
            return true;
        }
        false
    }

    fn wlan_enable(&mut self, cfg: &WlanEnableCfg, mode: i32, out: &mut Vec<Output>) {
        if self.busy_or_not_ready(out) {
            return;
        }
        // Cut to the message's maxima as ath10k_qmi_cfg_send_sync_msg() does; host_version and
        // shadow_reg_v2 stay unset, the other three are always present.
        let req = WlanCfgReq {
            host_version: None,
            tgt_cfg: Some(cfg.ce_tgt_cfg.iter().take(MAX_NUM_CE).copied().collect()),
            svc_cfg: Some(cfg.ce_svc_cfg.iter().take(MAX_NUM_SVC).copied().collect()),
            shadow_reg: Some(
                cfg.shadow_reg_cfg
                    .iter()
                    .take(MAX_NUM_SHADOW_REG)
                    .copied()
                    .collect(),
            ),
            shadow_reg_v2: None,
        };
        self.enable_mode = Some(mode);
        self.send(Step::WlanCfg, &req, out);
    }

    fn on_wlan_cfg(&mut self, buf: &[u8], out: &mut Vec<Output>) {
        let Some(r) = self.decode::<WlanCfgResp>(Step::WlanCfg, buf, out) else {
            return;
        };
        if !r.resp.is_success() {
            return self.rejected(Step::WlanCfg, r.resp.error, out);
        }
        out.push(Output::Fact(Fact::WlanConfigured));
        let mode = self.enable_mode.take().unwrap_or(driver_mode::MISSION);
        self.send_mode(mode, out);
    }

    fn wlan_mode(&mut self, mode: i32, out: &mut Vec<Output>) {
        if self.pending.is_some() {
            out.push(Output::Failed(Failure::Busy));
            return;
        }
        if !self.fw_ready {
            out.push(Output::Failed(Failure::NotReady));
            return;
        }
        self.send_mode(mode, out);
    }

    fn send_mode(&mut self, mode: i32, out: &mut Vec<Output>) {
        let req = WlanModeReq {
            mode,
            hw_debug: Some(0),
        };
        self.enable_mode = Some(mode);
        self.send(Step::WlanMode, &req, out);
    }

    fn on_wlan_mode(&mut self, buf: &[u8], out: &mut Vec<Output>) {
        let Some(r) = self.decode::<WlanModeResp>(Step::WlanMode, buf, out) else {
            return;
        };
        if !r.resp.is_success() {
            return self.rejected(Step::WlanMode, r.resp.error, out);
        }
        let mode = self.enable_mode.take().unwrap_or(driver_mode::MISSION);
        self.state = State::Ready;
        out.push(Output::Fact(Fact::ModeSet { mode }));
    }

    fn on_fw_log_mode(&mut self, buf: &[u8], out: &mut Vec<Output>) {
        let Some(r) = self.decode::<IniResp>(Step::FwLogMode, buf, out) else {
            return;
        };
        if !r.resp.is_success() {
            return self.rejected(Step::FwLogMode, r.resp.error, out);
        }
        self.state = State::Ready;
        let mode = self.log_mode.take().unwrap_or(0);
        out.push(Output::Fact(Fact::FwLogModeSet { mode }));
    }
}

/// The message id a response to this step's request carries.
fn step_msg_id(step: Step) -> u16 {
    match step {
        Step::IndRegister => msg_id::IND_REGISTER,
        Step::HostCap => msg_id::HOST_CAP,
        Step::MsaInfo => msg_id::MSA_INFO,
        Step::MsaReady => msg_id::MSA_READY,
        Step::Cap => msg_id::CAP,
        Step::BdfDownload => msg_id::BDF_DOWNLOAD,
        Step::CalReport => msg_id::CAL_REPORT,
        Step::WlanCfg => msg_id::WLAN_CFG,
        Step::WlanMode => msg_id::WLAN_MODE,
        Step::FwLogMode => msg_id::INI,
    }
}
