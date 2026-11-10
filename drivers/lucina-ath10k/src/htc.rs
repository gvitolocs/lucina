//! HTC, the host-target control layer that sits between the copy engines and WMI/HTT, for the WCN3990 on SNOC.
//!
//! Derived from Linux `drivers/net/wireless/ath/ath10k/htc.c` and `htc.h` (SPDX ISC; Copyright (c) 2005-2011
//! Atheros Communications Inc., 2011-2017 Qualcomm Atheros, Inc., 2022 Qualcomm Innovation Center, Inc., Qualcomm
//! Technologies, Inc. and/or its subsidiaries) and the `ATH10K_HW_WCN3990` entry of `core.c`'s `hw_params`
//! (`use_fw_tx_credits = false`, so the host starts with a single transmit credit for the WMI control service).
//!
//! * [`Hdr`] is `struct ath10k_htc_hdr`: eid, flags, payload length, the rx/tx byte pair (trailer length, sequence
//!   number) and the pad length. [`flags`] holds `ATH10K_HTC_FLAG_*`.
//! * [`svc`] holds the service ids; [`msg`] the control message ids and the wire encoders/decoders of the READY,
//!   CONNECT_SERVICE, CONNECT_SERVICE_RESP and SETUP_COMPLETE_EXT messages (`struct ath10k_htc_msg`).
//! * [`Htc`] is `struct ath10k_htc` plus the handshake of `ath10k_htc_init`, `ath10k_htc_wait_target`,
//!   `ath10k_htc_connect_service` and `ath10k_htc_start` as a pure state machine: it never waits and never touches
//!   a copy engine. Feed it the control messages the target sends ([`Htc::on_control_message`]) and send the
//!   packets it returns; credit accounting is [`Htc::prepare_tx`]/[`Htc::release_credit`] (`ath10k_htc_send`'s
//!   `consume_credit`/`release_credit`) and the receive side is [`Htc::rx_frame`] (`ath10k_htc_rx_completion_handler`)
//!   with [`Htc::process_trailer`] (`ath10k_htc_process_trailer`).
//!
//! `tests/fixtures.rs` checks sizes, offsets, constants and message bytes against a C oracle built from the real
//! `htc.h`, and runs the real `ath10k_htc_process_trailer` on trailer vectors to compare with this port.
//! One deliberate difference: the C accepts a trailer record whose length byte is merely not larger than the bytes
//! left, so a record can claim 4 bytes past the end of the trailer; here the record header plus its length must fit.

use alloc::vec::Vec;

use crate::snoc_config::{self, PipePair};

/// `ath10k_htc_svc_id` (`SVC(group, index)` = `group << 8 | index`).
pub mod svc {
    pub const RESERVED: u16 = 0x0000;
    /// The HTC pseudo control service on endpoint 0.
    pub const RSVD_CTRL: u16 = 0x0001;
    pub const WMI_CONTROL: u16 = 0x0100;
    pub const WMI_DATA_BE: u16 = 0x0101;
    pub const WMI_DATA_BK: u16 = 0x0102;
    pub const WMI_DATA_VI: u16 = 0x0103;
    pub const WMI_DATA_VO: u16 = 0x0104;
    pub const NMI_CONTROL: u16 = 0x0200;
    pub const NMI_DATA: u16 = 0x0201;
    pub const HTT_DATA_MSG: u16 = 0x0300;
    pub const HTT_DATA2_MSG: u16 = 0x0301;
    pub const HTT_DATA3_MSG: u16 = 0x0302;
    pub const HTT_LOG_MSG: u16 = 0x0600;
    pub const TEST_RAW_STREAMS: u16 = 0xfe00;
}

/// HTC header flags (`ath10k_htc_tx_flags`, `ath10k_htc_rx_flags`).
pub mod flags {
    pub const NEED_CREDIT_UPDATE: u8 = 0x01;
    pub const SEND_BUNDLE: u8 = 0x02;
    pub const RECV_1MORE_BLOCK: u8 = 0x01;
    pub const TRAILER_PRESENT: u8 = 0x02;
    /// `ATH10K_HTC_FLAG_BUNDLE_MASK` (bits 7:4) and `ATH10K_HTC_BUNDLE_EXTRA_MASK` (bits 3:2).
    pub const BUNDLE_MASK: u8 = 0xf0;
    pub const BUNDLE_EXTRA_MASK: u8 = 0x0c;
}

/// `HTC_HOST_MAX_MSG_PER_RX_BUNDLE`.
pub const HOST_MAX_MSG_PER_RX_BUNDLE: u8 = 32;
/// `ATH10K_HTC_EP_COUNT`.
pub const EP_COUNT: usize = 9;
/// `ATH10K_HTC_MAX_LEN`: header plus payload of one frame.
pub const MAX_LEN: usize = 4096;
/// `ATH10K_HTC_MAX_CTRL_MSG_LEN`.
pub const MAX_CTRL_MSG_LEN: usize = 256;
/// `sizeof(struct ath10k_htc_hdr)`.
pub const HDR_LEN: usize = 8;
/// `sizeof(struct ath10k_ath10k_htc_record_hdr)`.
pub const RECORD_HDR_LEN: usize = 4;
/// `ATH10K_HTC_MSG_READY_EXT_ALT_DATA_MASK`.
pub const READY_EXT_ALT_DATA_MASK: u16 = 0xfff;
/// `ATH10K_NUM_CONTROL_TX_BUFFERS`.
pub const NUM_CONTROL_TX_BUFFERS: i32 = 2;

/// `ath10k_htc_get_bundle_count`.
pub fn bundle_count(max_msgs: u8, hdr_flags: u8) -> u32 {
    let count = u32::from((hdr_flags & flags::BUNDLE_MASK) >> 4);
    let extra = if max_msgs > 16 {
        u32::from((hdr_flags & flags::BUNDLE_EXTRA_MASK) >> 2) << 4
    } else {
        0
    };
    count + extra
}

/// `struct ath10k_htc_hdr`. `control_byte0` is `trailer_len` on receive; `control_byte1` is `seq_no` on transmit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Hdr {
    pub eid: u8,
    pub flags: u8,
    /// Payload length (not counting the header).
    pub len: u16,
    pub control_byte0: u8,
    pub control_byte1: u8,
    pub pad_len: u16,
}

impl Hdr {
    pub fn to_le_bytes(&self) -> [u8; HDR_LEN] {
        let l = self.len.to_le_bytes();
        let p = self.pad_len.to_le_bytes();
        [
            self.eid,
            self.flags,
            l[0],
            l[1],
            self.control_byte0,
            self.control_byte1,
            p[0],
            p[1],
        ]
    }

    pub fn parse(b: &[u8]) -> Option<Hdr> {
        if b.len() < HDR_LEN {
            return None;
        }
        Some(Hdr {
            eid: b[0],
            flags: b[1],
            len: u16::from_le_bytes([b[2], b[3]]),
            control_byte0: b[4],
            control_byte1: b[5],
            pad_len: u16::from_le_bytes([b[6], b[7]]),
        })
    }

    /// `trailer_len`, the receive meaning of `control_byte0`.
    pub fn trailer_len(&self) -> u8 {
        self.control_byte0
    }
}

/// Control messages on endpoint 0 (`enum ath10k_ath10k_htc_msg_id` and the message structs).
pub mod msg {
    pub const READY_ID: u16 = 1;
    pub const CONNECT_SERVICE_ID: u16 = 2;
    pub const CONNECT_SERVICE_RESP_ID: u16 = 3;
    pub const SETUP_COMPLETE_ID: u16 = 4;
    pub const SETUP_COMPLETE_EX_ID: u16 = 5;
    pub const SEND_SUSPEND_COMPLETE: u16 = 6;

    /// `ath10k_htc_version`.
    pub const VERSION_2P0: u8 = 0;
    pub const VERSION_2P1: u8 = 1;

    /// `ath10k_htc_conn_flags`.
    pub const CONN_FLAGS_THRESHOLD_LEVEL_MASK: u16 = 0x3;
    pub const CONN_FLAGS_REDUCE_CREDIT_DRIBBLE: u16 = 1 << 2;
    pub const CONN_FLAGS_DISABLE_CREDIT_FLOW_CTRL: u16 = 1 << 3;
    pub const CONN_FLAGS_RECV_ALLOC_MASK: u16 = 0xff00;
    pub const CONN_FLAGS_RECV_ALLOC_LSB: u32 = 8;

    /// `ath10k_htc_conn_svc_status`.
    pub const CONN_SVC_STATUS_SUCCESS: u8 = 0;
    pub const CONN_SVC_STATUS_NOT_FOUND: u8 = 1;
    pub const CONN_SVC_STATUS_FAILED: u8 = 2;
    pub const CONN_SVC_STATUS_NO_RESOURCES: u8 = 3;
    pub const CONN_SVC_STATUS_NO_MORE_EP: u8 = 4;

    /// `ath10k_htc_setup_complete_flags`.
    pub const SETUP_COMPLETE_FLAGS_RX_BNDL_EN: u32 = 1;

    /// Sizes: message header (2) plus the union member (`sizeof(struct ath10k_htc_msg)` is 12, the largest).
    pub const HDR_LEN: usize = 2;
    pub const READY_LEN: usize = 6;
    pub const READY_EXT_LEN: usize = 10;
    pub const CONNECT_SERVICE_LEN: usize = 6;
    pub const CONNECT_SERVICE_RESP_LEN: usize = 6;
    pub const SETUP_COMPLETE_EXT_LEN: usize = 10;
}

/// `HTC_RECORD_*` ids of trailer records.
pub mod record {
    pub const NULL: u8 = 0;
    pub const CREDITS: u8 = 1;
    pub const LOOKAHEAD: u8 = 2;
    pub const LOOKAHEAD_BUNDLE: u8 = 3;
}

/// Errors of the HTC layer, with the errno the C uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HtcError {
    /// Malformed message or trailer (`-EINVAL`).
    Invalid,
    /// Control message of the wrong kind or size at the wrong time (`-ECOMM`, `-EPROTO`).
    Protocol,
    /// Not enough transmit credits yet (`-EAGAIN`).
    NoCredits,
    /// No such endpoint (`-ENOENT`).
    BadEndpoint,
    /// The call does not fit the handshake state.
    WrongState,
    /// The service is not served by any copy engine pair (`-ENOENT` from the HIF).
    NoPipe,
    /// The target refused the service; the status byte of the response.
    ConnectRefused(u8),
}

/// Frames the host must hand to a copy engine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutPacket {
    /// Source (host to target) copy engine.
    pub ul_pipe: u8,
    /// HTC header plus payload, ready for `CopyEngine::send`.
    pub bytes: Vec<u8>,
}

/// `struct ath10k_htc_ep`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub eid: u8,
    /// `svc::RESERVED` (= `ATH10K_HTC_SVC_ID_UNUSED`) while unconnected.
    pub service_id: u16,
    pub max_ep_message_len: u16,
    pub max_tx_queue_depth: i32,
    pub ul_pipe_id: u8,
    pub dl_pipe_id: u8,
    /// Next tx sequence number (debugging aid in the C, but it goes on the wire).
    pub seq_no: u8,
    pub tx_credits: u32,
    pub tx_credit_size: u32,
    pub tx_credit_flow_enabled: bool,
}

impl Endpoint {
    const fn unused(eid: u8) -> Endpoint {
        Endpoint {
            eid,
            service_id: svc::RESERVED,
            max_ep_message_len: 0,
            max_tx_queue_depth: 0,
            ul_pipe_id: 0,
            dl_pipe_id: 0,
            seq_no: 0,
            tx_credits: 0,
            tx_credit_size: 0,
            tx_credit_flow_enabled: true,
        }
    }
}

/// The result of the READY message (`ath10k_htc_wait_target`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ready {
    /// `credit_count` as the target sent it (WCN3990 ignores it: `use_fw_tx_credits` is false).
    pub target_credit_count: u16,
    pub total_transmit_credits: u32,
    pub target_credit_size: u32,
    pub max_endpoints: u8,
    /// Only with the extended message.
    pub extended: bool,
    pub htc_version: u8,
    pub alt_data_credit_size: u32,
    pub max_msgs_per_htc_bundle: u8,
}

/// Where the handshake stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// The pseudo control endpoint is connected; waiting for the target's READY message.
    WaitReady,
    /// READY seen; services can be connected and then [`Htc::start`] called.
    Ready,
    /// A CONNECT_SERVICE is outstanding.
    Connecting {
        service_id: u16,
        tx_alloc: u8,
        disable_credit_flow: bool,
    },
    /// SETUP_COMPLETE sent.
    Started,
}

/// A decoded frame for an endpoint, payload only (trailer removed).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RxFrame<'a> {
    pub eid: u8,
    pub payload: &'a [u8],
}

/// Why `rx_frame` dropped a frame (the C `goto out` paths; none of them is fatal).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RxDrop {
    /// Shorter than a header.
    Short,
    /// `eid >= ATH10K_HTC_EP_COUNT`.
    BadEid,
    /// The endpoint has no service.
    NotConnected,
    /// `payload_len + header > ATH10K_HTC_MAX_LEN`.
    TooLong,
    /// The buffer holds fewer bytes than the header promises.
    Truncated,
    /// Trailer length below a record header or above the payload.
    BadTrailerLen,
    /// The trailer did not parse.
    BadTrailer(HtcError),
    /// Nothing but a trailer.
    Empty,
}

/// Outcome of parsing a trailer: next-message lookaheads the target offered.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Lookaheads {
    pub entries: Vec<[u8; 4]>,
}

/// `struct ath10k_htc` for WCN3990.
#[derive(Clone, Debug)]
pub struct Htc {
    endpoints: [Endpoint; EP_COUNT],
    state: State,
    total_transmit_credits: u32,
    target_credit_size: u32,
    max_msgs_per_htc_bundle: u8,
    alt_data_credit_size: u32,
}

/// `use_fw_tx_credits` is false for WCN3990: one credit, whatever the target says.
const WCN3990_TOTAL_TRANSMIT_CREDITS: u32 = 1;

fn div_round_up(n: u32, d: u32) -> u32 {
    n.div_ceil(d)
}

impl Htc {
    /// `ath10k_htc_init`: reset the endpoints and connect the pseudo control service on endpoint 0 (no message is
    /// exchanged; it only needs a pipe pair).
    pub fn new() -> Result<Htc, HtcError> {
        let mut eps = [Endpoint::unused(0); EP_COUNT];
        for (i, e) in eps.iter_mut().enumerate() {
            *e = Endpoint::unused(i as u8);
        }
        let pipes =
            snoc_config::map_service_to_pipe(svc::RSVD_CTRL).map_err(|_| HtcError::NoPipe)?;
        let ep0 = &mut eps[0];
        ep0.service_id = svc::RSVD_CTRL;
        ep0.max_tx_queue_depth = NUM_CONTROL_TX_BUFFERS;
        ep0.max_ep_message_len = MAX_CTRL_MSG_LEN as u16;
        ep0.tx_credits = 0;
        ep0.tx_credit_size = 0;
        ep0.ul_pipe_id = pipes.ul_pipe;
        ep0.dl_pipe_id = pipes.dl_pipe;
        ep0.tx_credit_flow_enabled = false;
        Ok(Htc {
            endpoints: eps,
            state: State::WaitReady,
            total_transmit_credits: 0,
            target_credit_size: 0,
            max_msgs_per_htc_bundle: 0,
            alt_data_credit_size: 0,
        })
    }

    pub fn state(&self) -> State {
        self.state
    }

    pub fn endpoint(&self, eid: u8) -> Option<&Endpoint> {
        self.endpoints.get(usize::from(eid))
    }

    pub fn max_msgs_per_htc_bundle(&self) -> u8 {
        self.max_msgs_per_htc_bundle
    }

    /// `ath10k_htc_change_tx_credit_flow`.
    pub fn change_tx_credit_flow(&mut self, eid: u8, enable: bool) -> Result<(), HtcError> {
        self.endpoints
            .get_mut(usize::from(eid))
            .ok_or(HtcError::BadEndpoint)?
            .tx_credit_flow_enabled = enable;
        Ok(())
    }

    // ---- handshake ---------------------------------------------------------------------------------------------

    /// Feed one control message (the payload of a frame on endpoint 0, HTC header and trailer already removed).
    ///
    /// * In [`State::WaitReady`], a READY message (`ath10k_htc_wait_target`): at least 8 bytes, id 1, a non-zero
    ///   credit size; the extended form (12+ bytes) also carries the alternate data credit size and the receive
    ///   bundle limit. Returns [`Event::Ready`].
    /// * In [`State::Connecting`], the CONNECT_SERVICE_RESP (`ath10k_htc_connect_service`). Returns
    ///   [`Event::Connected`] or an error; a refused connect leaves the state machine in [`State::Ready`].
    /// * `SEND_SUSPEND_COMPLETE` is reported; anything else is ignored, as the C does (`ignoring unsolicited htc
    ///   ep0 event`).
    pub fn on_control_message(&mut self, payload: &[u8]) -> Result<Event, HtcError> {
        // the C copies at most 256 bytes
        let m = &payload[..payload.len().min(MAX_CTRL_MSG_LEN)];
        if m.len() < msg::HDR_LEN {
            return Ok(Event::Ignored);
        }
        let id = u16::from_le_bytes([m[0], m[1]]);
        match (self.state, id) {
            (State::WaitReady, msg::READY_ID) => self.handle_ready(m).map(Event::Ready),
            (
                State::Connecting {
                    service_id,
                    tx_alloc,
                    disable_credit_flow,
                },
                msg::CONNECT_SERVICE_RESP_ID,
            ) => {
                let r = self.handle_connect_response(m, service_id, tx_alloc, disable_credit_flow);
                if r.is_err() {
                    self.state = State::Ready;
                }
                r.map(Event::Connected)
            }
            (_, msg::SEND_SUSPEND_COMPLETE) => Ok(Event::SuspendComplete),
            _ => Ok(Event::Ignored),
        }
    }

    fn handle_ready(&mut self, m: &[u8]) -> Result<Ready, HtcError> {
        if m.len() < msg::HDR_LEN + msg::READY_LEN {
            return Err(HtcError::Protocol);
        }
        let credit_count = u16::from_le_bytes([m[2], m[3]]);
        let credit_size = u32::from(u16::from_le_bytes([m[4], m[5]]));
        let max_endpoints = m[6];
        if credit_size == 0 {
            return Err(HtcError::Protocol);
        }
        let mut ready = Ready {
            target_credit_count: credit_count,
            total_transmit_credits: WCN3990_TOTAL_TRANSMIT_CREDITS,
            target_credit_size: credit_size,
            max_endpoints,
            extended: false,
            htc_version: 0,
            alt_data_credit_size: 0,
            max_msgs_per_htc_bundle: 0,
        };
        // the only way to tell the extended message is its size
        if m.len() >= msg::HDR_LEN + msg::READY_EXT_LEN {
            let reserved = u16::from_le_bytes([m[10], m[11]]);
            ready.extended = true;
            ready.htc_version = m[8];
            ready.alt_data_credit_size = u32::from(reserved & READY_EXT_ALT_DATA_MASK);
            ready.max_msgs_per_htc_bundle = m[9].min(HOST_MAX_MSG_PER_RX_BUNDLE);
        }
        self.total_transmit_credits = ready.total_transmit_credits;
        self.target_credit_size = ready.target_credit_size;
        if ready.extended {
            self.alt_data_credit_size = ready.alt_data_credit_size;
            self.max_msgs_per_htc_bundle = ready.max_msgs_per_htc_bundle;
        }
        self.state = State::Ready;
        Ok(ready)
    }

    /// `ath10k_htc_get_credit_allocation`: only the WMI control service has credit flow control, with all credits.
    fn credit_allocation(&self, service_id: u16) -> u8 {
        if service_id == svc::WMI_CONTROL {
            self.total_transmit_credits as u8
        } else {
            0
        }
    }

    /// First half of `ath10k_htc_connect_service`: the CONNECT_SERVICE packet to send on endpoint 0. Only valid
    /// in [`State::Ready`]. The pseudo control service is connected by [`Htc::new`], not here.
    pub fn connect_service(&mut self, service_id: u16) -> Result<OutPacket, HtcError> {
        if self.state != State::Ready {
            return Err(HtcError::WrongState);
        }
        if service_id == svc::RSVD_CTRL || service_id == svc::RESERVED {
            return Err(HtcError::Invalid);
        }
        let tx_alloc = self.credit_allocation(service_id);
        let mut conn_flags: u16 = (u16::from(tx_alloc) << msg::CONN_FLAGS_RECV_ALLOC_LSB)
            & msg::CONN_FLAGS_RECV_ALLOC_MASK;
        let mut disable_credit_flow = false;
        // only enable credit flow control for the WMI control service
        if service_id != svc::WMI_CONTROL {
            conn_flags |= msg::CONN_FLAGS_DISABLE_CREDIT_FLOW_CTRL;
            disable_credit_flow = true;
        }
        let mut payload = Vec::with_capacity(msg::HDR_LEN + msg::CONNECT_SERVICE_LEN);
        payload.extend_from_slice(&msg::CONNECT_SERVICE_ID.to_le_bytes());
        payload.extend_from_slice(&service_id.to_le_bytes());
        payload.extend_from_slice(&conn_flags.to_le_bytes());
        payload.extend_from_slice(&[0, 0]);
        let pkt = self.frame_for(0, &payload)?;
        self.state = State::Connecting {
            service_id,
            tx_alloc,
            disable_credit_flow,
        };
        Ok(pkt)
    }

    fn handle_connect_response(
        &mut self,
        m: &[u8],
        service_id: u16,
        tx_alloc: u8,
        disable_credit_flow: bool,
    ) -> Result<Connected, HtcError> {
        if m.len() < msg::HDR_LEN + msg::CONNECT_SERVICE_RESP_LEN {
            return Err(HtcError::Protocol);
        }
        let resp_service = u16::from_le_bytes([m[2], m[3]]);
        let status = m[4];
        let eid = m[5];
        let max_msg_size = u16::from_le_bytes([m[6], m[7]]);
        if status != msg::CONN_SVC_STATUS_SUCCESS {
            return Err(HtcError::ConnectRefused(status));
        }
        // the C does not compare the echoed service id; neither do we, but report it
        let _ = resp_service;
        if usize::from(eid) >= EP_COUNT || max_msg_size == 0 {
            return Err(HtcError::Protocol);
        }
        if self.endpoints[usize::from(eid)].service_id != svc::RESERVED {
            return Err(HtcError::Protocol);
        }
        // the C configures the endpoint and then asks the HIF for pipes, leaving it half set up on failure;
        // look the pipes up first instead
        let pipes: PipePair =
            snoc_config::map_service_to_pipe(service_id).map_err(|_| HtcError::NoPipe)?;
        let ep = &mut self.endpoints[usize::from(eid)];
        ep.eid = eid;
        ep.service_id = service_id;
        ep.max_tx_queue_depth = 0;
        ep.max_ep_message_len = max_msg_size;
        ep.tx_credits = u32::from(tx_alloc);
        ep.tx_credit_size = self.target_credit_size;
        if service_id == svc::HTT_DATA_MSG && self.alt_data_credit_size != 0 {
            ep.tx_credit_size = self.alt_data_credit_size;
        }
        ep.ul_pipe_id = pipes.ul_pipe;
        ep.dl_pipe_id = pipes.dl_pipe;
        if disable_credit_flow && ep.tx_credit_flow_enabled {
            ep.tx_credit_flow_enabled = false;
        }
        self.state = State::Ready;
        Ok(Connected {
            eid,
            service_id,
            max_msg_len: max_msg_size,
            ul_pipe: pipes.ul_pipe,
            dl_pipe: pipes.dl_pipe,
        })
    }

    /// `ath10k_htc_start`: the SETUP_COMPLETE_EXT packet (all-zero flags: no receive bundling off SDIO). Afterwards
    /// the pktlog service would be connected if the SNOC table served it; it does not (`pktlog_supported`).
    pub fn start(&mut self) -> Result<OutPacket, HtcError> {
        if self.state != State::Ready {
            return Err(HtcError::WrongState);
        }
        let mut payload = alloc::vec![0u8; msg::HDR_LEN + msg::SETUP_COMPLETE_EXT_LEN];
        payload[0..2].copy_from_slice(&msg::SETUP_COMPLETE_EX_ID.to_le_bytes());
        let pkt = self.frame_for(0, &payload)?;
        self.state = State::Started;
        Ok(pkt)
    }

    /// `ath10k_htc_pktlog_svc_supported`: whether the HTT_LOG service maps to a pipe pair. On the WCN3990 SNOC
    /// tables it has only a target-to-host entry, so this is false and the driver never connects it.
    pub fn pktlog_supported() -> bool {
        snoc_config::map_service_to_pipe(svc::HTT_LOG_MSG).is_ok()
    }

    // ---- transmit ----------------------------------------------------------------------------------------------

    /// `ath10k_htc_consume_credit`: credits needed for `len` bytes (header included), taken only if available.
    fn consume_credit(&mut self, eid: u8, len: u32) -> Result<(), HtcError> {
        let ep = &mut self.endpoints[usize::from(eid)];
        if !ep.tx_credit_flow_enabled {
            return Ok(());
        }
        if ep.tx_credit_size == 0 {
            return Err(HtcError::NoCredits);
        }
        let credits = div_round_up(len, ep.tx_credit_size);
        if ep.tx_credits < credits {
            return Err(HtcError::NoCredits);
        }
        ep.tx_credits -= credits;
        Ok(())
    }

    /// `ath10k_htc_release_credit`: give back the credits of a send that failed after `prepare_tx`.
    /// `len` is the total length including the header.
    pub fn release_credit(&mut self, eid: u8, len: u32) {
        if let Some(ep) = self.endpoints.get_mut(usize::from(eid)) {
            if !ep.tx_credit_flow_enabled || ep.tx_credit_size == 0 {
                return;
            }
            ep.tx_credits = ep
                .tx_credits
                .saturating_add(div_round_up(len, ep.tx_credit_size));
        }
    }

    /// The core of `ath10k_htc_send`: take credits for `payload_len` bytes plus the header, and return the header
    /// to put in front (with the next sequence number). On `NoCredits` nothing changes.
    pub fn prepare_tx(&mut self, eid: u8, payload_len: usize) -> Result<Hdr, HtcError> {
        if usize::from(eid) >= EP_COUNT {
            return Err(HtcError::BadEndpoint);
        }
        let total = payload_len + HDR_LEN;
        if total > MAX_LEN || payload_len > usize::from(u16::MAX) {
            return Err(HtcError::Invalid);
        }
        if self.endpoints[usize::from(eid)].service_id == svc::RESERVED {
            return Err(HtcError::BadEndpoint);
        }
        self.consume_credit(eid, total as u32)?;
        let ep = &mut self.endpoints[usize::from(eid)];
        let mut hdr = Hdr {
            eid,
            len: payload_len as u16,
            ..Hdr::default()
        };
        if ep.tx_credit_flow_enabled {
            hdr.flags |= flags::NEED_CREDIT_UPDATE;
        }
        hdr.control_byte1 = ep.seq_no;
        ep.seq_no = ep.seq_no.wrapping_add(1);
        Ok(hdr)
    }

    /// Header plus payload for `eid`, via [`Htc::prepare_tx`].
    pub fn frame_for(&mut self, eid: u8, payload: &[u8]) -> Result<OutPacket, HtcError> {
        let hdr = self.prepare_tx(eid, payload.len())?;
        let mut bytes = Vec::with_capacity(HDR_LEN + payload.len());
        bytes.extend_from_slice(&hdr.to_le_bytes());
        bytes.extend_from_slice(payload);
        Ok(OutPacket {
            ul_pipe: self.endpoints[usize::from(eid)].ul_pipe_id,
            bytes,
        })
    }

    // ---- receive -----------------------------------------------------------------------------------------------

    /// `ath10k_htc_process_trailer`: walk the records, adding credits to endpoints and collecting lookaheads.
    ///
    /// * credit records (id 1): `len` at least 4; each 4-byte report adds `credits` to endpoint `eid` and an
    ///   endpoint id of 9 or more ends the record early (not the trailer);
    /// * lookahead (id 2): `len` at least 12; used only if `pre_valid == !post_valid`, silently ignored otherwise;
    /// * lookahead bundle (id 3): `len / 4` entries, between 1 and the bundle limit;
    /// * other ids are skipped.
    pub fn process_trailer(&mut self, buf: &[u8]) -> Result<Lookaheads, HtcError> {
        let mut la = Lookaheads::default();
        let mut rest = buf;
        while !rest.is_empty() {
            if rest.len() < RECORD_HDR_LEN {
                return Err(HtcError::Invalid);
            }
            let id = rest[0];
            let len = usize::from(rest[1]);
            // the C only checks len against the bytes left including the header; require the record to fit
            if RECORD_HDR_LEN + len > rest.len() {
                return Err(HtcError::Invalid);
            }
            let body = &rest[RECORD_HDR_LEN..RECORD_HDR_LEN + len];
            match id {
                record::CREDITS => {
                    if len < 4 {
                        return Err(HtcError::Invalid);
                    }
                    for report in body.as_chunks::<4>().0 {
                        let (eid, credits) = (report[0], report[1]);
                        if usize::from(eid) >= EP_COUNT {
                            break;
                        }
                        let ep = &mut self.endpoints[usize::from(eid)];
                        ep.tx_credits = ep.tx_credits.saturating_add(u32::from(credits));
                    }
                }
                record::LOOKAHEAD => {
                    if len < 12 {
                        return Err(HtcError::Invalid);
                    }
                    let (pre_valid, post_valid) = (body[0], body[8]);
                    if pre_valid == !post_valid {
                        // the C overwrites the caller's single slot; collect instead
                        la.entries.clear();
                        la.entries.push([body[4], body[5], body[6], body[7]]);
                    }
                }
                record::LOOKAHEAD_BUNDLE => {
                    let cnt = len / 4;
                    if cnt == 0 || cnt > usize::from(self.max_msgs_per_htc_bundle) {
                        return Err(HtcError::Invalid);
                    }
                    la.entries.clear();
                    for e in body.as_chunks::<4>().0.iter().take(cnt) {
                        la.entries.push([e[0], e[1], e[2], e[3]]);
                    }
                }
                _ => {}
            }
            rest = &rest[RECORD_HDR_LEN + len..];
        }
        Ok(la)
    }

    /// `ath10k_htc_rx_completion_handler`: validate one frame from a receive pipe and split off the payload.
    /// Credit reports in the trailer are applied. The caller delivers `payload` to the endpoint's owner (endpoint
    /// 0 payloads go to [`Htc::on_control_message`]).
    pub fn rx_frame<'a>(&mut self, frame: &'a [u8]) -> Result<RxFrame<'a>, RxDrop> {
        let hdr = Hdr::parse(frame).ok_or(RxDrop::Short)?;
        let body = &frame[HDR_LEN..];
        if usize::from(hdr.eid) >= EP_COUNT {
            return Err(RxDrop::BadEid);
        }
        if self.endpoints[usize::from(hdr.eid)].service_id == svc::RESERVED {
            return Err(RxDrop::NotConnected);
        }
        let payload_len = usize::from(hdr.len);
        if payload_len + HDR_LEN > MAX_LEN {
            return Err(RxDrop::TooLong);
        }
        if body.len() < payload_len {
            return Err(RxDrop::Truncated);
        }
        let mut trailer_len = 0usize;
        if hdr.flags & flags::TRAILER_PRESENT != 0 {
            trailer_len = usize::from(hdr.trailer_len());
            if trailer_len < RECORD_HDR_LEN || trailer_len > payload_len {
                return Err(RxDrop::BadTrailerLen);
            }
            // the trailer is the last `trailer_len` bytes of the payload
            let trailer = &body[payload_len - trailer_len..payload_len];
            self.process_trailer(trailer).map_err(RxDrop::BadTrailer)?;
        }
        if payload_len == trailer_len {
            // zero length packet with trailer data, just drop these
            return Err(RxDrop::Empty);
        }
        Ok(RxFrame {
            eid: hdr.eid,
            payload: &body[..payload_len - trailer_len],
        })
    }
}

/// What a control message meant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Ready(Ready),
    Connected(Connected),
    SuspendComplete,
    Ignored,
}

/// A connected service (`struct ath10k_htc_svc_conn_resp` and the pipes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Connected {
    pub eid: u8,
    pub service_id: u16,
    pub max_msg_len: u16,
    pub ul_pipe: u8,
    pub dl_pipe: u8,
}
