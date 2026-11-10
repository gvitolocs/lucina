// Derived from Linux drivers/rpmsg/qcom_glink_native.c (v7.1) and, for the channel names it binds, net/qrtr/smd.c.
// Those files are GPL-2.0 (Copyright (c) 2016-2017 Linaro Ltd; (c) 2015 Sony Mobile Communications Inc. and
// (c) 2013 The Linux Foundation for smd.c). This is a re-implementation as a non-blocking state machine in Rust,
// not a copy of the C; it stays under the same licence.

//! The native GLINK protocol over a [`Fifo`] pair: a state machine with no threads, no sleeping and no locks.
//!
//! How the C is turned inside out:
//!
//! * `qcom_glink_native_rx` (the interrupt handler) is [`Glink::poll`]. It drains the receive FIFO, then writes
//!   whatever is queued for transmission, and returns the [`Event`]s that happened. Linux defers VERSION,
//!   VERSION_ACK, OPEN, CLOSE, CLOSE_ACK and RX_INTENT_REQ to a workqueue (`rx_defer`, `qcom_glink_work`) and
//!   handles the rest in the interrupt; here everything is handled in arrival order, which also removes the race
//!   where data for a channel overtakes its deferred OPEN.
//! * `qcom_glink_tx` blocks (up to 10 s a round, forever overall) until the ring has room. Here every command is
//!   encoded into a queue and written by the next flush; a flush happens at the end of every public call. Control
//!   commands go out strictly in the order they were queued. When the ring is full the writer sends one
//!   `READ_NOTIF` into the reserve, like `qcom_glink_tx`'s `sent_read_notify`, and the remote's doorbell brings the
//!   next `poll`. The doorbell is rung once per flush that wrote something, not once per frame; the data is in the
//!   ring before the bell, so the remote sees the same thing.
//! * `__qcom_glink_send` (rpmsg `send`, which waits) is [`Glink::send`], which copies the message into a per-channel
//!   queue and returns. Choosing a remote intent, asking for one with `RX_INTENT_REQ` when there is none, and
//!   cutting the message into 8 KiB chunks happen as the queue drains. A message the remote cannot take fails later,
//!   as an [`Event::SendFailed`], where the C returned `-EAGAIN`, `-ETIMEDOUT` or `-ECANCELED`.
//! * Timeouts are the caller's clock: [`Glink::tick`] takes the time in milliseconds. The intent request waits 10 s
//!   (`wait_event_timeout(.., 10 * HZ)`), an open waits 5 s for the ack and 5 s for the remote's own open
//!   (`qcom_glink_create_local`, `qcom_glink_create_remote`). Nothing else has a timer.
//!
//! ## How a channel opens
//!
//! The channel the QRTR driver binds is named `IPCRTR` (`qrtr/smd.c`: `qcom_smd_qrtr_smd_match`, `MODULE_ALIAS`).
//! Linux never opens it itself. The modem sends `OPEN`; `qcom_glink_rx_open` finds no local channel of that name,
//! allocates one, records the remote id and registers an rpmsg device named after the channel. The rpmsg core then
//! binds a driver whose `id_table` has that name, and the driver's probe calls `rpmsg_create_ept`, which is
//! `qcom_glink_create_ept`; because the channel is already in the remote-id table that is
//! `qcom_glink_create_remote`, and only then does Linux answer:
//!
//! 1. `OPEN_ACK` for the remote's id, then
//! 2. its own `OPEN` (a new local id), then wait up to 5 s for
//! 3. the remote's `OPEN_ACK` (`qcom_glink_rx_open_ack` completes `open_ack`). On timeout it sends `CLOSE` to undo
//!    step 1.
//!
//! After the probe returns, the rpmsg core calls `announce_create` (`qcom_glink_announce_create`), which advertises
//! the default receive intents with one `INTENT` command each: `qcom,intents` from the device tree node, or 5
//! intents of 1 KiB, reusable. The SC7180 modem `glink-edge` node has no `qcom,intents`, so the default is what
//! applies. No driver, no answer: an `OPEN` for a name nobody binds is never acked. Here the "driver" is the caller:
//! names in [`Config::auto_accept`] (default `IPCRTR`) are accepted at once, any other name produces an
//! [`Event::Offered`] and waits for [`Glink::accept`] or [`Glink::open`].
//!
//! The other order, a local open (`qcom_glink_create_local`), is: `OPEN`, wait for `OPEN_ACK` (5 s), wait for the
//! remote's `OPEN` (5 s; `qcom_glink_rx_open` then finds the channel by name among the *local* ids and records the
//! remote id), then `OPEN_ACK` for it, then the same default intents. [`Glink::open`] does this unless the remote has
//! already offered that name, in which case it is the remote-initiated order above, as in `qcom_glink_create_ept`.
//!
//! ## Closing
//!
//! `CLOSE` carries the sender's id and is answered by `CLOSE_ACK` carrying the receiver's id. When the remote
//! closes, Linux removes the rpmsg device, which makes the bound driver's endpoint go away, which sends Linux's own
//! `CLOSE` (`qcom_glink_destroy_ept`), and only then sends `CLOSE_ACK` (`qcom_glink_rx_close`). So the wire order is
//! `CLOSE` in, `CLOSE` out, `CLOSE_ACK` out, `CLOSE_ACK` in. This code does the same for every channel, not only
//! the ones Linux made an rpmsg device for.
//!
//! ## Flow control
//!
//! The sender may only write a message into an intent the receiver advertised (`INTENT`: size and id). It picks the
//! smallest free one that fits, preferring an exact fit, and marks it in use until `RX_DONE` (the receiver finished
//! with it; the intent is gone) or `RX_DONE_W_REUSE` (it may be used again). With none free it sends
//! `RX_INTENT_REQ`; the receiver allocates an intent of that size, advertises it and answers `RX_INTENT_REQ_ACK`.
//! The receiving side does the same for the remote's requests. After a message is delivered the intent is released
//! at once, not when the caller is done with the bytes (the C calls the endpoint callback synchronously and
//! releases the intent right after it returns).
//!
//! ## Hardening that is not in the C
//!
//! A malformed peer must not hang or panic. The C stops the whole receive loop on an unknown command and on a
//! message for an unknown intent (the same message then comes back on every interrupt), loops forever in
//! `qcom_glink_handle_intent` when an `INTENT` is not fully in the FIFO, and would wait for ever for a message
//! larger than the ring. Here an unknown command is skipped as an 8-byte message, a message that is incomplete is
//! waited for, and one that cannot ever fit in the ring (or too many bad commands in a row) is a [`ResetReason`]:
//! the link is torn down as for a remote crash and negotiation starts again.

use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::vec::Vec;
use core::mem;

use crate::fifo::{Doorbell, Fault, Fifo, Layout, LayoutError, Shared};
use crate::wire::{
    self, CMD_CLOSE, CMD_CLOSE_ACK, CMD_INTENT, CMD_OPEN, CMD_OPEN_ACK, CMD_READ_NOTIF, CMD_RX_DONE,
};
use crate::wire::{
    CMD_RX_DONE_W_REUSE, CMD_RX_INTENT_REQ, CMD_RX_INTENT_REQ_ACK, CMD_SIGNALS, CMD_TX_DATA,
};
use crate::wire::{
    CMD_TX_DATA_CONT, CMD_VERSION, CMD_VERSION_ACK, DATA_HDR_LEN, HDR_LEN, Header, align8,
};

/// How long an `RX_INTENT_REQ` may stay unanswered (`wait_event_timeout(.., 10 * HZ)` in
/// `qcom_glink_request_intent`).
pub const INTENT_REQUEST_TIMEOUT_MS: u64 = 10_000;
/// How long each step of opening a channel may take (`wait_for_completion_timeout(.., 5 * HZ)`).
pub const OPEN_TIMEOUT_MS: u64 = 5_000;
/// The biggest chunk of a message in one `TX_DATA` (`SZ_8K` in `__qcom_glink_send`, for the waiting variant).
pub const MAX_CHUNK: usize = 8 * 1024;

/// VERSION frames sent before giving up. The C answers a remote that keeps refusing with another VERSION forever.
const MAX_VERSION_ATTEMPTS: u32 = 8;
/// Unknown commands in a row before the stream is declared garbage.
const MAX_BAD_STREAK: u32 = 8;
/// `RX_INTENT_REQ`s for one message before giving up on the remote's intents being big enough.
const MAX_INTENT_REQUESTS: u32 = 4;
/// Upper bound on commands handled per poll, a second guard behind "every command consumes 8 bytes or more".
const RX_LOOP_LIMIT: usize = 1 << 16;

/// A handle for a channel; it stays valid (and never refers to another channel) until a peer reset.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChannelId(pub u32);

/// Knobs; the defaults are what `qcom_glink_smem_register` and `qcom_glink_announce_create` use.
#[derive(Clone, Debug)]
pub struct Config {
    /// The features offered in VERSION (`GLINK_FEATURE_INTENT_REUSE`, as `qcom_glink_smem_register` passes).
    pub features: u32,
    /// Names accepted without asking, as a bound rpmsg driver's `id_table` would. Default: `IPCRTR`.
    pub auto_accept: Vec<String>,
    /// The intents advertised when a channel opens, as (size, count) groups. Default: 5 of 1 KiB.
    pub intents: Vec<(usize, usize)>,
    /// The largest intent this side grants to an `RX_INTENT_REQ` (the C allocates whatever is asked for).
    pub max_intent_size: usize,
    /// The most receive intents live on one channel (the C has no limit).
    pub max_intents_per_channel: usize,
    /// The most intents of the remote tracked per channel.
    pub max_remote_intents: usize,
    /// The longest message [`Glink::send`] takes.
    pub max_message: usize,
    /// The most bytes queued in one channel waiting for an intent or ring room.
    pub tx_queue_limit: usize,
    /// The most channels at once.
    pub max_channels: usize,
    /// Control frames waiting for ring room beyond which the receive FIFO is left alone (back-pressure).
    pub max_ctl_backlog: usize,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            features: wire::FEATURE_INTENT_REUSE,
            auto_accept: alloc::vec![String::from("IPCRTR")],
            intents: alloc::vec![(1024, 5)],
            max_intent_size: 256 * 1024,
            max_intents_per_channel: 32,
            max_remote_intents: 64,
            max_message: 256 * 1024,
            tx_queue_limit: 1024 * 1024,
            max_channels: 32,
            max_ctl_backlog: 128,
        }
    }
}

/// Where version negotiation stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkState {
    /// The shared indexes were junk or re-initialised; nothing is sent until they look sane again.
    Down,
    /// Our VERSION is out and not yet acked with matching features.
    Negotiating,
    /// The remote acked our VERSION with the features we hold.
    Up,
    /// The remote answered version 0 (it supports none of ours), or kept refusing.
    Rejected,
}

/// What a channel is doing, as far as this side can tell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelState {
    /// The remote sent `OPEN` and nobody has accepted it yet.
    Offered,
    /// The open handshake is in progress.
    Opening,
    Open,
    /// We sent `CLOSE` and wait for `CLOSE_ACK`.
    Closing,
}

/// Why the link was torn down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResetReason {
    /// The caller said so ([`Glink::reset`]), e.g. on a remote-processor crash notification.
    Requested,
    /// A shared index was junk, was overwritten, or the remote consumed data that was never written.
    Fault(Fault),
    /// The receive stream cannot be parsed: a message too big for the ring, or garbage commands in a row.
    Corrupt,
}

/// Why an open did not complete.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenFailure {
    Timeout,
}

/// Why a queued message was dropped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendFailure {
    /// The remote answered `RX_INTENT_REQ` with "denied" (the C's `-EAGAIN`).
    IntentDenied,
    /// No answer to `RX_INTENT_REQ` in 10 s (`-ETIMEDOUT`).
    Timeout,
    /// The remote granted intents but none was big enough, four requests in a row.
    IntentTooSmall,
    /// The channel was closed with the message still queued.
    Closed,
}

/// Something that happened, returned by [`Glink::poll`] and [`Glink::tick`] in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// Version negotiation finished (the remote's VERSION_ACK matched our features).
    LinkUp { features: u32 },
    /// The remote has no version in common.
    LinkRejected,
    /// The remote opened a channel nobody has accepted; see [`Glink::accept`].
    Offered { channel: ChannelId, name: String },
    /// Both directions are open; the default intents have been queued.
    ChannelOpened { channel: ChannelId, name: String },
    /// One complete message (all fragments reassembled).
    Data { channel: ChannelId, data: Vec<u8> },
    /// The remote set signal bits (`qcom_glink_handle_signals`): `enable` is `DSR | CTS` being set.
    Signals {
        channel: ChannelId,
        signals: u32,
        enable: bool,
    },
    /// The channel is gone or going: the remote closed it, our `CLOSE` was acked, or the link was reset.
    ChannelClosed { channel: ChannelId },
    OpenFailed {
        channel: ChannelId,
        reason: OpenFailure,
    },
    SendFailed {
        channel: ChannelId,
        len: usize,
        reason: SendFailure,
    },
    /// The link was torn down: all channels were closed (an event each, before this one) and VERSION was or will
    /// be sent again.
    PeerReset { reason: ResetReason },
}

/// Why a call was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    UnknownChannel,
    /// The channel is not open (or is closing) for this.
    NotOpen,
    /// Empty, longer than 31 bytes, or not UTF-8 without NULs.
    InvalidName,
    /// A name already opened locally.
    AlreadyOpen,
    /// Empty, or longer than [`Config::max_message`].
    InvalidLength,
    /// [`Config::tx_queue_limit`] reached; retry after the queue drains.
    QueueFull,
    TooManyChannels,
    /// All 65535 local channel ids are in use.
    NoFreeId,
}

struct LocalIntent {
    size: usize,
    reuse: bool,
    buf: Vec<u8>,
    /// A multi-fragment message is partly in `buf`.
    in_progress: bool,
    /// The rest of the current message is discarded (it overflowed the intent or arrived out of order).
    dropping: bool,
}

struct RemoteIntent {
    size: usize,
    in_use: bool,
}

/// `intent_req_result` and `intent_received` of `struct glink_channel`.
enum Req {
    Idle,
    Pending {
        since: u64,
        granted: Option<bool>,
        intent_seen: bool,
    },
}

struct Cur {
    iid: u32,
    offset: usize,
}

struct Channel {
    id: ChannelId,
    name: String,
    lcid: Option<u16>,
    rcid: Option<u16>,
    /// The remote acked our `OPEN` (`open_ack` completed).
    local_acked: bool,
    /// We sent `OPEN_ACK` for the remote's `OPEN`.
    ack_sent: bool,
    close_sent: bool,
    opened: bool,
    closed_reported: bool,
    deadline: Option<u64>,
    liids: BTreeMap<u32, LocalIntent>,
    next_liid: u32,
    riids: BTreeMap<u32, RemoteIntent>,
    req: Req,
    req_attempts: u32,
    txq: VecDeque<Vec<u8>>,
    txq_bytes: usize,
    cur: Option<Cur>,
}

impl Channel {
    fn new(id: ChannelId, name: String) -> Channel {
        Channel {
            id,
            name,
            lcid: None,
            rcid: None,
            local_acked: false,
            ack_sent: false,
            close_sent: false,
            opened: false,
            closed_reported: false,
            deadline: None,
            liids: BTreeMap::new(),
            next_liid: 1,
            riids: BTreeMap::new(),
            req: Req::Idle,
            req_attempts: 0,
            txq: VecDeque::new(),
            txq_bytes: 0,
            cur: None,
        }
    }

    fn state(&self) -> ChannelState {
        if self.close_sent {
            ChannelState::Closing
        } else if self.opened {
            ChannelState::Open
        } else if self.lcid.is_none() {
            ChannelState::Offered
        } else {
            ChannelState::Opening
        }
    }

    /// Drops everything that belonged to the open channel (intents, queued messages).
    fn clear_traffic(&mut self) {
        self.liids.clear();
        self.riids.clear();
        self.txq.clear();
        self.txq_bytes = 0;
        self.cur = None;
        self.req = Req::Idle;
    }

    /// The remote intent `__qcom_glink_send` would pick for `len` bytes: the smallest free one that fits, the first
    /// of equal sizes, stopping early at an exact fit.
    fn pick_intent(&self, len: usize) -> Option<u32> {
        let mut best: Option<(u32, usize)> = None;
        for (&iid, it) in &self.riids {
            if it.size >= len && !it.in_use {
                if best.is_none_or(|(_, size)| size > it.size) {
                    best = Some((iid, it.size));
                }
                if it.size == len {
                    break;
                }
            }
        }
        best.map(|(iid, _)| iid)
    }
}

enum Rx {
    Continue,
    /// The message is not complete yet; stop until the next poll.
    NeedMore,
    Corrupt,
}

/// One GLINK edge.
pub struct Glink<T: Shared + Doorbell> {
    io: T,
    fifo: Fifo,
    cfg: Config,
    link: LinkState,
    features: u32,
    version_attempts: u32,
    ctl: VecDeque<Vec<u8>>,
    channels: Vec<Channel>,
    next_id: u32,
    next_lcid: u32,
    events: VecDeque<Event>,
    now: u64,
    sent_read_notify: bool,
    bad_streak: u32,
    violations: u32,
    rx_more: bool,
    /// Whether the next resync keeps what the remote already wrote (after its restart) or drops it (after garbage).
    keep_unread: bool,
}

/// `ALIGN(sizeof(hdr) + chunk, 8)` and friends, for sizes that came off the wire.
fn frame_len(n: u64) -> u64 {
    (n + 7) & !7
}

impl<T: Shared + Doorbell> Glink<T> {
    /// `qcom_glink_native_probe` over `qcom_glink_smem_register`'s pipes: clears our indexes and queues VERSION.
    pub fn new(io: T, layout: Layout) -> Result<Glink<T>, LayoutError> {
        Glink::with_config(io, layout, Config::default())
    }

    pub fn with_config(mut io: T, layout: Layout, cfg: Config) -> Result<Glink<T>, LayoutError> {
        let fifo = Fifo::new(&mut io, layout)?;
        let mut g = Glink {
            io,
            fifo,
            features: cfg.features,
            cfg,
            link: LinkState::Negotiating,
            version_attempts: 0,
            ctl: VecDeque::new(),
            channels: Vec::new(),
            next_id: 1,
            next_lcid: wire::CID_MIN,
            events: VecDeque::new(),
            now: 0,
            sent_read_notify: false,
            bad_streak: 0,
            violations: 0,
            rx_more: false,
            keep_unread: false,
        };
        g.send_version();
        g.flush();
        Ok(g)
    }

    // ---- inspection -------------------------------------------------------------------------------------------

    pub fn link_state(&self) -> LinkState {
        self.link
    }

    /// The features currently agreed (what `glink->features` holds).
    pub fn features(&self) -> u32 {
        self.features
    }

    pub fn channel_state(&self, id: ChannelId) -> Option<ChannelState> {
        self.channels
            .iter()
            .find(|c| c.id == id)
            .map(Channel::state)
    }

    pub fn channel_name(&self, id: ChannelId) -> Option<&str> {
        self.channels
            .iter()
            .find(|c| c.id == id)
            .map(|c| c.name.as_str())
    }

    /// A live channel by name.
    pub fn find_channel(&self, name: &str) -> Option<ChannelId> {
        self.channels
            .iter()
            .find(|c| c.name == name && !c.close_sent)
            .map(|c| c.id)
    }

    /// Messages from the remote that were malformed or referred to nothing (each one is dropped, as the C logs and
    /// drops it).
    pub fn violations(&self) -> u32 {
        self.violations
    }

    /// True when the last [`Glink::poll`] left receive data behind (back-pressure or the per-poll limit); poll again.
    pub fn rx_pending(&self) -> bool {
        self.rx_more
    }

    /// Control frames and data messages still waiting to be written.
    pub fn tx_backlog(&self) -> usize {
        self.ctl.len() + self.channels.iter().map(|c| c.txq.len()).sum::<usize>()
    }

    /// The earliest time [`Glink::tick`] has something to expire, if any.
    pub fn next_deadline(&self) -> Option<u64> {
        self.channels
            .iter()
            .flat_map(|c| {
                let req = match c.req {
                    Req::Pending { since, .. } => Some(since + INTENT_REQUEST_TIMEOUT_MS),
                    Req::Idle => None,
                };
                [c.deadline, req]
            })
            .flatten()
            .min()
    }

    pub fn transport(&mut self) -> &mut T {
        &mut self.io
    }

    // ---- driving ----------------------------------------------------------------------------------------------

    /// The receive interrupt (`qcom_glink_smem_intr` -> `qcom_glink_native_rx`), and the place pending output is
    /// written. Never blocks and always returns.
    pub fn poll(&mut self) -> Vec<Event> {
        if self.link == LinkState::Down {
            self.recover();
        }
        if self.link != LinkState::Down {
            self.rx_loop();
        }
        self.flush();
        self.events.drain(..).collect()
    }

    /// Advances the clock to `now_ms` (any monotonic millisecond count), expires what has timed out and flushes.
    pub fn tick(&mut self, now_ms: u64) -> Vec<Event> {
        self.now = self.now.max(now_ms);
        let now = self.now;
        for i in 0..self.channels.len() {
            let c = &mut self.channels[i];
            if c.deadline.is_some_and(|d| now >= d) {
                c.deadline = None;
                if !c.opened && !c.close_sent {
                    // The C forgets the channel and leaves the remote's half open; sending CLOSE is tidier.
                    if let Some(lcid) = c.lcid {
                        self.ctl.push_back(wire::simple(CMD_CLOSE, lcid, 0));
                        c.close_sent = true;
                    }
                    c.closed_reported = true;
                    self.events.push_back(Event::OpenFailed {
                        channel: c.id,
                        reason: OpenFailure::Timeout,
                    });
                }
            }
            if let Req::Pending { since, .. } = c.req
                && now >= since + INTENT_REQUEST_TIMEOUT_MS
            {
                c.req = Req::Idle;
                Self::fail_front(c, &mut self.events, SendFailure::Timeout);
            }
        }
        self.flush();
        self.events.drain(..).collect()
    }

    /// Tears the link down as for a remote crash (what `qcom_glink_native_remove` followed by a new probe does after
    /// a remoteproc restart), and starts negotiation again.
    pub fn reset(&mut self) -> Vec<Event> {
        self.do_reset(ResetReason::Requested);
        self.flush();
        self.events.drain(..).collect()
    }

    // ---- channels ---------------------------------------------------------------------------------------------

    /// Opens a channel by name. If the remote has already offered that name this accepts it (the remote-initiated
    /// order); otherwise it starts a local open. Completion is [`Event::ChannelOpened`].
    pub fn open(&mut self, name: &str) -> Result<ChannelId, Error> {
        if name.contains('\0') || validate_name(name.as_bytes()).is_none() {
            return Err(Error::InvalidName);
        }
        if let Some(i) = self
            .channels
            .iter()
            .position(|c| c.name == name && c.lcid.is_none() && c.rcid.is_some())
        {
            self.accept_at(i)?;
            self.flush();
            return Ok(self.channels[i].id);
        }
        if self
            .channels
            .iter()
            .any(|c| c.name == name && c.lcid.is_some() && !c.close_sent)
        {
            return Err(Error::AlreadyOpen);
        }
        if self.channels.len() >= self.cfg.max_channels {
            return Err(Error::TooManyChannels);
        }
        let lcid = self.alloc_lcid()?;
        let id = self.new_id();
        let mut c = Channel::new(id, String::from(name));
        c.lcid = Some(lcid);
        c.deadline = Some(self.now + OPEN_TIMEOUT_MS);
        self.channels.push(c);
        self.ctl.push_back(wire::open(lcid, name));
        self.flush();
        Ok(id)
    }

    /// Accepts a channel the remote offered: the work `qcom_glink_create_remote` does.
    pub fn accept(&mut self, id: ChannelId) -> Result<(), Error> {
        let i = self.index(id)?;
        self.accept_at(i)?;
        self.flush();
        Ok(())
    }

    /// `qcom_glink_destroy_ept`: sends `CLOSE`. The channel is gone for good when `CLOSE_ACK` arrives.
    pub fn close(&mut self, id: ChannelId) -> Result<(), Error> {
        let i = self.index(id)?;
        let c = &mut self.channels[i];
        let Some(lcid) = c.lcid else {
            return Err(Error::NotOpen);
        };
        if c.close_sent {
            return Ok(());
        }
        c.close_sent = true;
        c.deadline = None;
        self.ctl.push_back(wire::simple(CMD_CLOSE, lcid, 0));
        let mut dropped = Vec::new();
        for m in c.txq.drain(..) {
            dropped.push(m.len());
        }
        c.txq_bytes = 0;
        c.cur = None;
        c.req = Req::Idle;
        for len in dropped {
            self.events.push_back(Event::SendFailed {
                channel: id,
                len,
                reason: SendFailure::Closed,
            });
        }
        self.flush();
        Ok(())
    }

    /// Queues `data` as one message. It is written as soon as the remote has an intent for it and the ring has
    /// room; failure to get an intent is reported later as [`Event::SendFailed`].
    pub fn send(&mut self, id: ChannelId, data: &[u8]) -> Result<(), Error> {
        let i = self.index(id)?;
        let c = &mut self.channels[i];
        if !c.opened || c.close_sent || c.rcid.is_none() {
            return Err(Error::NotOpen);
        }
        if data.is_empty() || data.len() > self.cfg.max_message {
            return Err(Error::InvalidLength);
        }
        if c.txq_bytes + data.len() > self.cfg.tx_queue_limit {
            return Err(Error::QueueFull);
        }
        c.txq_bytes += data.len();
        c.txq.push_back(data.to_vec());
        self.flush();
        Ok(())
    }

    /// Sends raw signal bits (`SIGNALS`, `qcom_glink_set_flow_control`'s message).
    pub fn send_signals(&mut self, id: ChannelId, signals: u32) -> Result<(), Error> {
        let i = self.index(id)?;
        let c = &self.channels[i];
        let (Some(lcid), true) = (c.lcid, c.opened && !c.close_sent) else {
            return Err(Error::NotOpen);
        };
        self.ctl.push_back(wire::simple(CMD_SIGNALS, lcid, signals));
        self.flush();
        Ok(())
    }

    /// `qcom_glink_set_flow_control`. Surprising as it reads: `pause` sends `DTR | RTS` (the bits [`wire::SIG_DSR`]
    /// and [`wire::SIG_CTS`]) and not pausing sends 0, while the receiving side, `qcom_glink_handle_signals`, treats
    /// any of those bits set as `enable`. This reproduces the sender as it is.
    pub fn set_flow_control(&mut self, id: ChannelId, pause: bool) -> Result<(), Error> {
        self.send_signals(
            id,
            if pause {
                wire::SIG_DSR | wire::SIG_CTS
            } else {
                0
            },
        )
    }

    // ---- internals: ids ---------------------------------------------------------------------------------------

    fn index(&self, id: ChannelId) -> Result<usize, Error> {
        self.channels
            .iter()
            .position(|c| c.id == id)
            .ok_or(Error::UnknownChannel)
    }

    fn new_id(&mut self) -> ChannelId {
        let id = ChannelId(self.next_id);
        self.next_id = self.next_id.wrapping_add(1);
        id
    }

    /// `idr_alloc_cyclic(&glink->lcids, .., RPM_GLINK_CID_MIN, RPM_GLINK_CID_MAX, ..)`.
    fn alloc_lcid(&mut self) -> Result<u16, Error> {
        let span = wire::CID_MAX - wire::CID_MIN;
        for _ in 0..span {
            let cand = self.next_lcid;
            self.next_lcid = if cand + 1 >= wire::CID_MAX {
                wire::CID_MIN
            } else {
                cand + 1
            };
            if !self.channels.iter().any(|c| c.lcid == Some(cand as u16)) {
                return Ok(cand as u16);
            }
        }
        Err(Error::NoFreeId)
    }

    fn by_rcid(&self, rcid: u16) -> Option<usize> {
        self.channels.iter().position(|c| c.rcid == Some(rcid))
    }

    fn by_lcid(&self, lcid: u16) -> Option<usize> {
        self.channels.iter().position(|c| c.lcid == Some(lcid))
    }

    fn violation(&mut self) {
        self.violations = self.violations.saturating_add(1);
    }

    // ---- internals: negotiation -------------------------------------------------------------------------------

    /// `qcom_glink_send_version`.
    fn send_version(&mut self) {
        if self.link == LinkState::Rejected {
            return;
        }
        if self.version_attempts >= MAX_VERSION_ATTEMPTS {
            self.link = LinkState::Rejected;
            self.events.push_back(Event::LinkRejected);
            return;
        }
        self.version_attempts += 1;
        self.ctl
            .push_back(wire::simple(CMD_VERSION, wire::VERSION_1, self.features));
    }

    /// `qcom_glink_send_version_ack`.
    fn send_version_ack(&mut self) {
        self.ctl.push_back(wire::simple(
            CMD_VERSION_ACK,
            wire::VERSION_1,
            self.features,
        ));
    }

    /// `qcom_glink_receive_version`: the remote proposes. Version 1 narrows the features to the common set; version
    /// 0 is ignored; anything newer is answered with our version 1 and unchanged features (the downgrade).
    fn on_version(&mut self, version: u16, features: u32) {
        match version {
            0 => {}
            wire::VERSION_1 => {
                self.features &= features;
                self.send_version_ack();
            }
            _ => self.send_version_ack(),
        }
    }

    /// `qcom_glink_receive_version_ack`: the remote answers our proposal. Equal features end it; otherwise narrow
    /// and propose again, as does an answer in a version we do not speak.
    fn on_version_ack(&mut self, version: u16, features: u32) {
        match version {
            0 => {
                if self.link != LinkState::Rejected {
                    self.link = LinkState::Rejected;
                    self.events.push_back(Event::LinkRejected);
                }
            }
            wire::VERSION_1 if features == self.features => {
                self.link = LinkState::Up;
                self.version_attempts = 0;
                self.events.push_back(Event::LinkUp {
                    features: self.features,
                });
            }
            wire::VERSION_1 => {
                self.features &= features;
                self.send_version();
            }
            _ => self.send_version(),
        }
    }

    // ---- internals: channels ----------------------------------------------------------------------------------

    /// The remote-initiated order of `qcom_glink_create_remote`: `OPEN_ACK`, then our `OPEN`.
    fn accept_at(&mut self, i: usize) -> Result<(), Error> {
        let (rcid, lcid_set) = (self.channels[i].rcid, self.channels[i].lcid.is_some());
        let Some(rcid) = rcid else {
            return Err(Error::NotOpen);
        };
        if lcid_set {
            return Ok(());
        }
        let lcid = self.alloc_lcid()?;
        let c = &mut self.channels[i];
        self.ctl.push_back(wire::simple(CMD_OPEN_ACK, rcid, 0));
        c.ack_sent = true;
        c.lcid = Some(lcid);
        c.deadline = Some(self.now + OPEN_TIMEOUT_MS);
        self.ctl.push_back(wire::open(lcid, &c.name));
        Ok(())
    }

    /// Both ends known and acked: the channel is open. Sends `OPEN_ACK` first if the local-open order still owes it
    /// (`qcom_glink_create_local`'s last step), then advertises the default intents (`qcom_glink_announce_create`).
    fn progress_open(&mut self, i: usize) {
        let c = &mut self.channels[i];
        if c.opened || c.close_sent || !c.local_acked {
            return;
        }
        let (Some(lcid), Some(rcid)) = (c.lcid, c.rcid) else {
            return;
        };
        if !c.ack_sent {
            self.ctl.push_back(wire::simple(CMD_OPEN_ACK, rcid, 0));
            c.ack_sent = true;
        }
        c.opened = true;
        c.deadline = None;
        self.events.push_back(Event::ChannelOpened {
            channel: c.id,
            name: c.name.clone(),
        });
        let reuse = self.features & wire::FEATURE_INTENT_REUSE != 0;
        for gi in 0..self.cfg.intents.len() {
            let (size, count) = self.cfg.intents[gi];
            for _ in 0..count {
                let Some(iid) = self.alloc_intent(i, size, reuse) else {
                    break;
                };
                self.ctl.push_back(wire::intent(lcid, size as u32, iid));
            }
        }
    }

    /// `qcom_glink_alloc_intent`, with the limits this code adds. Returns the new intent id.
    fn alloc_intent(&mut self, i: usize, size: usize, reuse: bool) -> Option<u32> {
        let c = &mut self.channels[i];
        if size > self.cfg.max_intent_size || c.liids.len() >= self.cfg.max_intents_per_channel {
            return None;
        }
        // idr_alloc_cyclic(&channel->liids, intent, 1, -1, ..)
        let mut id = c.next_liid;
        for _ in 0..=c.liids.len() {
            if id == 0 || id >= i32::MAX as u32 {
                id = 1;
            }
            if !c.liids.contains_key(&id) {
                break;
            }
            id += 1;
        }
        c.next_liid = id + 1;
        c.liids.insert(
            id,
            LocalIntent {
                size,
                reuse,
                buf: Vec::new(),
                in_progress: false,
                dropping: false,
            },
        );
        Some(id)
    }

    /// `qcom_glink_rx_open`, after the deferred-command copy: the remote opened `name` as its channel `rcid`.
    fn on_open(&mut self, rcid: u16, name_bytes: &[u8]) {
        let Some(name) = validate_name(name_bytes) else {
            self.violation();
            return;
        };
        if self.by_rcid(rcid).is_some() {
            // idr_alloc(&glink->rcids, .., rcid, rcid + 1) fails: "Unable to insert channel into rcid list".
            self.violation();
            return;
        }
        // The local-open order: we opened this name already, so the remote's OPEN completes it.
        if let Some(i) = self
            .channels
            .iter()
            .position(|c| c.name == name && c.lcid.is_some() && c.rcid.is_none() && !c.close_sent)
        {
            self.channels[i].rcid = Some(rcid);
            self.progress_open(i);
            return;
        }
        if self.channels.len() >= self.cfg.max_channels {
            self.violation();
            return;
        }
        let id = self.new_id();
        let mut c = Channel::new(id, String::from(name));
        c.rcid = Some(rcid);
        self.channels.push(c);
        let i = self.channels.len() - 1;
        if self.cfg.auto_accept.iter().any(|n| n == name) {
            // No free local id is the only way this fails; the offer then stays open for a later `accept`.
            if self.accept_at(i).is_err() {
                self.events.push_back(Event::Offered {
                    channel: id,
                    name: String::from(name),
                });
            }
        } else {
            self.events.push_back(Event::Offered {
                channel: id,
                name: String::from(name),
            });
        }
    }

    /// `qcom_glink_rx_open_ack`.
    fn on_open_ack(&mut self, lcid: u16) {
        let Some(i) = self.by_lcid(lcid) else {
            self.violation();
            return;
        };
        let now = self.now;
        let c = &mut self.channels[i];
        if c.local_acked {
            self.violation();
            return;
        }
        c.local_acked = true;
        if c.rcid.is_none() {
            // Now waiting for the remote's own OPEN: the second 5 s.
            c.deadline = Some(now + OPEN_TIMEOUT_MS);
        }
        self.progress_open(i);
    }

    fn report_closed(&mut self, i: usize) {
        let c = &mut self.channels[i];
        if !c.closed_reported {
            c.closed_reported = true;
            self.events
                .push_back(Event::ChannelClosed { channel: c.id });
        }
    }

    /// `qcom_glink_rx_close`: the remote closes. Our own `CLOSE` (what unbinding the rpmsg device causes) goes out
    /// before the `CLOSE_ACK`.
    fn on_close(&mut self, rcid: u16) {
        let Some(i) = self.by_rcid(rcid) else {
            self.violation();
            return;
        };
        self.report_closed(i);
        let c = &mut self.channels[i];
        c.clear_traffic();
        c.deadline = None;
        if let (Some(lcid), false) = (c.lcid, c.close_sent) {
            self.ctl.push_back(wire::simple(CMD_CLOSE, lcid, 0));
            c.close_sent = true;
        }
        self.ctl.push_back(wire::simple(CMD_CLOSE_ACK, rcid, 0));
        c.rcid = None;
        if c.lcid.is_none() {
            self.channels.remove(i);
        }
    }

    /// `qcom_glink_rx_close_ack`.
    fn on_close_ack(&mut self, lcid: u16) {
        let Some(i) = self.by_lcid(lcid) else {
            self.violation();
            return;
        };
        self.report_closed(i);
        let c = &mut self.channels[i];
        c.lcid = None;
        c.clear_traffic();
        if c.rcid.is_none() {
            self.channels.remove(i);
        }
    }

    // ---- internals: receive -----------------------------------------------------------------------------------

    /// Handles faults and runs the command loop of `qcom_glink_native_rx`.
    fn rx_loop(&mut self) {
        self.rx_more = false;
        for _ in 0..RX_LOOP_LIMIT {
            if self.ctl.len() >= self.cfg.max_ctl_backlog {
                self.rx_more = true;
                return;
            }
            let avail = match self.fifo.rx_avail(&mut self.io) {
                Ok(a) => a,
                Err(f) => {
                    self.do_reset(ResetReason::Fault(f));
                    return;
                }
            };
            if avail < HDR_LEN {
                return;
            }
            let mut raw = [0u8; HDR_LEN];
            self.fifo.rx_peek(&mut self.io, 0, &mut raw);
            let h = Header::decode(&raw);
            match self.handle(h, avail) {
                Rx::Continue => {}
                Rx::NeedMore => return,
                Rx::Corrupt => {
                    self.do_reset(ResetReason::Corrupt);
                    return;
                }
            }
        }
        self.rx_more = true;
    }

    /// Consumes `count` bytes, never more than are there (a remote that does not pad its last frame).
    fn advance(&mut self, count: usize, avail: usize) {
        self.fifo.rx_advance(&mut self.io, count.min(avail));
    }

    fn handle(&mut self, h: Header, avail: usize) -> Rx {
        if !matches!(
            h.cmd,
            CMD_VERSION
                | CMD_VERSION_ACK
                | CMD_OPEN
                | CMD_CLOSE
                | CMD_OPEN_ACK
                | CMD_INTENT
                | CMD_RX_DONE
                | CMD_RX_INTENT_REQ
                | CMD_RX_INTENT_REQ_ACK
                | CMD_TX_DATA
                | CMD_CLOSE_ACK
                | CMD_TX_DATA_CONT
                | CMD_READ_NOTIF
                | CMD_RX_DONE_W_REUSE
                | CMD_SIGNALS
        ) {
            // `dev_err(.., "unhandled rx cmd")`; the C then stops and sees the same bytes at every interrupt.
            self.violation();
            self.bad_streak += 1;
            if self.bad_streak >= MAX_BAD_STREAK {
                return Rx::Corrupt;
            }
            self.advance(HDR_LEN, avail);
            return Rx::Continue;
        }
        self.bad_streak = 0;
        match h.cmd {
            CMD_OPEN => return self.rx_open(h, avail),
            CMD_TX_DATA | CMD_TX_DATA_CONT => return self.rx_data(h, avail),
            CMD_INTENT => return self.rx_intent(h, avail),
            _ => {}
        }
        // Everything else is the bare 8-byte header.
        self.advance(HDR_LEN, avail);
        match h.cmd {
            CMD_VERSION => self.on_version(h.param1, h.param2),
            CMD_VERSION_ACK => self.on_version_ack(h.param1, h.param2),
            CMD_OPEN_ACK => self.on_open_ack(h.param1),
            CMD_CLOSE => self.on_close(h.param1),
            CMD_CLOSE_ACK => self.on_close_ack(h.param1),
            CMD_RX_INTENT_REQ => self.on_intent_req(h.param1, h.param2),
            CMD_RX_INTENT_REQ_ACK => self.on_intent_req_ack(h.param1, h.param2),
            CMD_RX_DONE => self.on_rx_done(h.param1, h.param2, false),
            CMD_RX_DONE_W_REUSE => self.on_rx_done(h.param1, h.param2, true),
            CMD_SIGNALS => self.on_signals(h.param1, h.param2),
            // `qcom_glink_rx_read_notif`: the remote is out of room and wants to hear that we read.
            CMD_READ_NOTIF => self.io.ring(),
            _ => {}
        }
        Rx::Continue
    }

    /// `GLINK_CMD_OPEN` through `qcom_glink_rx_defer(glink, param2 & 0xffff)`: the name follows the header.
    fn rx_open(&mut self, h: Header, avail: usize) -> Rx {
        let name_len = (h.param2 & 0xffff) as usize;
        let total = HDR_LEN + align8(name_len);
        if total > self.fifo.rx_capacity() {
            return Rx::Corrupt;
        }
        if avail < total {
            return Rx::NeedMore;
        }
        let mut name = alloc::vec![0u8; name_len];
        self.fifo.rx_peek(&mut self.io, HDR_LEN, &mut name);
        self.advance(total, avail);
        self.on_open(h.param1, &name);
        Rx::Continue
    }

    /// `qcom_glink_handle_intent`: `param2` intents follow the header, each (size, id).
    fn rx_intent(&mut self, h: Header, avail: usize) -> Rx {
        let total = frame_len(HDR_LEN as u64 + 8 * u64::from(h.param2));
        if total > self.fifo.rx_capacity() as u64 {
            return Rx::Corrupt;
        }
        let total = total as usize;
        if avail < total {
            // The C returns here without consuming anything and loops on the same message for ever.
            return Rx::NeedMore;
        }
        let mut body = alloc::vec![0u8; 8 * h.param2 as usize];
        self.fifo.rx_peek(&mut self.io, HDR_LEN, &mut body);
        self.advance(total, avail);
        let Some(i) = self.by_rcid(h.param1) else {
            self.violation();
            return Rx::Continue;
        };
        let max = self.cfg.max_remote_intents;
        let c = &mut self.channels[i];
        for pair in body.as_chunks::<8>().0 {
            let size = u32::from_le_bytes([pair[0], pair[1], pair[2], pair[3]]) as usize;
            let iid = u32::from_le_bytes([pair[4], pair[5], pair[6], pair[7]]);
            // idr_alloc(&channel->riids, intent, iid, iid + 1, ..) refuses an id already there.
            if c.riids.len() >= max || c.riids.contains_key(&iid) {
                self.violations = self.violations.saturating_add(1);
                continue;
            }
            c.riids.insert(
                iid,
                RemoteIntent {
                    size,
                    in_use: false,
                },
            );
        }
        if let Req::Pending { intent_seen, .. } = &mut c.req {
            *intent_seen = true;
        }
        Rx::Continue
    }

    /// `qcom_glink_rx_data`.
    fn rx_data(&mut self, h: Header, avail: usize) -> Rx {
        if avail < DATA_HDR_LEN {
            return Rx::NeedMore;
        }
        let mut raw = [0u8; DATA_HDR_LEN];
        self.fifo.rx_peek(&mut self.io, 0, &mut raw);
        let chunk = u32::from_le_bytes([raw[8], raw[9], raw[10], raw[11]]) as usize;
        let left = u32::from_le_bytes([raw[12], raw[13], raw[14], raw[15]]);
        let need = DATA_HDR_LEN as u64 + chunk as u64;
        if frame_len(need) > self.fifo.rx_capacity() as u64 {
            // The C would wait for ever for a payload that cannot fit in the ring.
            return Rx::Corrupt;
        }
        if (avail as u64) < need {
            return Rx::NeedMore;
        }
        let adv = frame_len(need) as usize;
        let rcid = h.param1;
        let iid = h.param2;

        let Some(i) = self.by_rcid(rcid) else {
            // "Data on non-existing channel": dropped.
            self.violation();
            self.advance(adv, avail);
            return Rx::Continue;
        };
        let c = &mut self.channels[i];
        let Some(it) = c.liids.get_mut(&iid) else {
            // "no intent found for channel": dropped (the C also stops its loop here).
            self.violations = self.violations.saturating_add(1);
            self.advance(adv, avail);
            return Rx::Continue;
        };
        let mut bad = false;
        if h.cmd == CMD_TX_DATA {
            // A new message starts; whatever was half there is abandoned.
            if it.in_progress {
                it.buf.clear();
                it.in_progress = false;
                bad = true;
            }
            it.dropping = false;
        } else if !it.in_progress && !it.dropping {
            // A continuation with nothing to continue.
            it.dropping = true;
            bad = true;
        }
        if !it.dropping {
            if it.size - it.buf.len() < chunk {
                // "Insufficient space in intent": the header lied; drop this message.
                it.dropping = true;
                it.buf.clear();
                it.in_progress = false;
                bad = true;
            } else {
                let old = it.buf.len();
                it.buf.resize(old + chunk, 0);
                self.fifo
                    .rx_peek(&mut self.io, DATA_HDR_LEN, &mut it.buf[old..]);
                it.in_progress = left != 0;
            }
        }
        let done = left == 0;
        let reuse = it.reuse;
        let delivered = if done && !it.dropping {
            Some(mem::take(&mut it.buf))
        } else {
            None
        };
        if done {
            it.dropping = false;
            it.buf.clear();
            it.in_progress = false;
        }
        let (id, closing, lcid) = (c.id, c.close_sent, c.lcid);
        if done {
            // `qcom_glink_rx_done`: a one-shot intent leaves the table, then RX_DONE says it was consumed.
            if !reuse {
                c.liids.remove(&iid);
            }
            if let Some(lcid) = lcid {
                let cmd = if reuse {
                    CMD_RX_DONE_W_REUSE
                } else {
                    CMD_RX_DONE
                };
                self.ctl.push_back(wire::simple(cmd, lcid, iid));
            }
            if let (Some(data), false) = (delivered, closing) {
                self.events.push_back(Event::Data { channel: id, data });
            }
        }
        if bad {
            self.violation();
        }
        self.advance(adv, avail);
        Rx::Continue
    }

    /// `qcom_glink_handle_intent_req`: allocate, advertise, then answer.
    fn on_intent_req(&mut self, rcid: u16, size: u32) {
        let Some(i) = self.by_rcid(rcid) else {
            self.violation();
            return;
        };
        let Some(lcid) = self.channels[i].lcid else {
            self.violation();
            return;
        };
        let granted = match self.alloc_intent(i, size as usize, false) {
            Some(iid) => {
                self.ctl.push_back(wire::intent(lcid, size, iid));
                true
            }
            None => false,
        };
        self.ctl.push_back(wire::simple(
            CMD_RX_INTENT_REQ_ACK,
            lcid,
            u32::from(granted),
        ));
    }

    /// `qcom_glink_handle_intent_req_ack`.
    fn on_intent_req_ack(&mut self, rcid: u16, granted: u32) {
        let Some(i) = self.by_rcid(rcid) else {
            self.violation();
            return;
        };
        if let Req::Pending { granted: g, .. } = &mut self.channels[i].req {
            *g = Some(granted != 0);
        }
    }

    /// `qcom_glink_handle_rx_done`.
    fn on_rx_done(&mut self, rcid: u16, iid: u32, reuse: bool) {
        let Some(i) = self.by_rcid(rcid) else {
            self.violation();
            return;
        };
        let c = &mut self.channels[i];
        let Some(it) = c.riids.get_mut(&iid) else {
            self.violation();
            return;
        };
        it.in_use = false;
        if !reuse {
            c.riids.remove(&iid);
        }
    }

    /// `qcom_glink_handle_signals`.
    fn on_signals(&mut self, rcid: u16, signals: u32) {
        let Some(i) = self.by_rcid(rcid) else {
            self.violation();
            return;
        };
        let enable = signals & (wire::SIG_DSR | wire::SIG_CTS) != 0;
        let channel = self.channels[i].id;
        self.events.push_back(Event::Signals {
            channel,
            signals,
            enable,
        });
    }

    // ---- internals: transmit ----------------------------------------------------------------------------------

    /// Drops the oldest queued message of `c` and reports why.
    fn fail_front(c: &mut Channel, events: &mut VecDeque<Event>, reason: SendFailure) {
        if let Some(m) = c.txq.pop_front() {
            c.txq_bytes -= m.len();
            c.cur = None;
            c.req_attempts = 0;
            events.push_back(Event::SendFailed {
                channel: c.id,
                len: m.len(),
                reason,
            });
        }
    }

    /// Settles intent requests that were answered.
    fn resolve_requests(&mut self) {
        for c in &mut self.channels {
            match c.req {
                Req::Pending {
                    granted: Some(false),
                    ..
                } => {
                    c.req = Req::Idle;
                    Self::fail_front(c, &mut self.events, SendFailure::IntentDenied);
                }
                Req::Pending {
                    granted: Some(true),
                    intent_seen: true,
                    ..
                } => c.req = Req::Idle,
                _ => {}
            }
        }
    }

    /// Writes queued control frames, then queued data, until the ring is full. Rings the doorbell if it wrote.
    fn flush(&mut self) {
        if self.link == LinkState::Down {
            return;
        }
        self.resolve_requests();
        let mut wrote = false;
        let mut blocked = match self.drain_ctl(&mut wrote) {
            Ok(b) => b,
            Err(fault) => {
                self.do_reset(ResetReason::Fault(fault));
                return;
            }
        };
        if !blocked {
            for i in 0..self.channels.len() {
                match self.pump(i, &mut wrote) {
                    Ok(true) => {
                        blocked = true;
                        break;
                    }
                    Ok(false) => {}
                    Err(fault) => {
                        self.do_reset(ResetReason::Fault(fault));
                        return;
                    }
                }
            }
            // Pumping may have queued an RX_INTENT_REQ; it goes out in this flush, not the next.
            if !blocked && !self.ctl.is_empty() {
                match self.drain_ctl(&mut wrote) {
                    Ok(b) => blocked = b,
                    Err(fault) => {
                        self.do_reset(ResetReason::Fault(fault));
                        return;
                    }
                }
            }
        }
        if blocked {
            // `qcom_glink_tx`: once per stall, tell the remote we are waiting for room.
            if !self.sent_read_notify {
                let notif = wire::simple(CMD_READ_NOTIF, 0, 0);
                match self.fifo.tx_write(&mut self.io, &[&notif], true) {
                    Ok(true) => {
                        self.sent_read_notify = true;
                        wrote = true;
                    }
                    Ok(false) => {}
                    Err(fault) => {
                        self.do_reset(ResetReason::Fault(fault));
                        return;
                    }
                }
            }
        } else {
            self.sent_read_notify = false;
        }
        if wrote {
            self.io.ring();
        }
    }

    /// Writes control frames in order until the queue is empty (`Ok(false)`) or the ring is full (`Ok(true)`).
    fn drain_ctl(&mut self, wrote: &mut bool) -> Result<bool, Fault> {
        while let Some(f) = self.ctl.front() {
            if !self.fifo.tx_write(&mut self.io, &[f], false)? {
                return Ok(true);
            }
            self.ctl.pop_front();
            *wrote = true;
        }
        Ok(false)
    }

    /// Moves the queued messages of one channel into the ring as far as intents and room allow. `Ok(true)` means the
    /// ring is full.
    fn pump(&mut self, i: usize, wrote: &mut bool) -> Result<bool, Fault> {
        let Self {
            io,
            fifo,
            channels,
            ctl,
            events,
            now,
            ..
        } = self;
        let c = &mut channels[i];
        // Room for the data header and a chunk, after the reserves, rounded down to the frame alignment.
        let max_chunk = ((fifo.tx_capacity() - DATA_HDR_LEN) & !7).min(MAX_CHUNK);
        loop {
            let Some(len) = c.txq.front().map(Vec::len) else {
                return Ok(false);
            };
            if c.cur.is_none() {
                if matches!(c.req, Req::Pending { .. }) {
                    // `qcom_glink_request_intent` is still waiting for its answer.
                    return Ok(false);
                }
                match c.pick_intent(len) {
                    Some(iid) => {
                        if let Some(it) = c.riids.get_mut(&iid) {
                            it.in_use = true;
                        }
                        c.cur = Some(Cur { iid, offset: 0 });
                        c.req_attempts = 0;
                    }
                    None => {
                        let Some(lcid) = c.lcid else {
                            return Ok(false);
                        };
                        if c.req_attempts >= MAX_INTENT_REQUESTS {
                            c.req_attempts = 0;
                            Self::fail_front(c, events, SendFailure::IntentTooSmall);
                            continue;
                        }
                        c.req_attempts += 1;
                        c.req = Req::Pending {
                            since: *now,
                            granted: None,
                            intent_seen: false,
                        };
                        // `qcom_glink_request_intent`: { id: u16, cid: u16, size: u32 }
                        ctl.push_back(wire::simple(CMD_RX_INTENT_REQ, lcid, len as u32));
                        return Ok(false);
                    }
                }
            }
            let (Some(cur), Some(lcid)) = (c.cur.as_mut(), c.lcid) else {
                return Ok(false);
            };
            let msg = &c.txq[0];
            while cur.offset < len {
                let chunk = (len - cur.offset).min(max_chunk);
                let cmd = if cur.offset == 0 {
                    CMD_TX_DATA
                } else {
                    CMD_TX_DATA_CONT
                };
                let left = (len - cur.offset - chunk) as u32;
                let hdr = wire::data_header(cmd, lcid, cur.iid, chunk as u32, left);
                if !fifo.tx_write(io, &[&hdr, &msg[cur.offset..cur.offset + chunk]], false)? {
                    return Ok(true);
                }
                *wrote = true;
                cur.offset += chunk;
            }
            if let Some(m) = c.txq.pop_front() {
                c.txq_bytes -= m.len();
            }
            c.cur = None;
        }
    }

    // ---- internals: reset -------------------------------------------------------------------------------------

    /// The remote is gone or the stream is unusable: close everything, report, and start over.
    fn do_reset(&mut self, reason: ResetReason) {
        for c in mem::take(&mut self.channels) {
            if !c.closed_reported {
                self.events
                    .push_back(Event::ChannelClosed { channel: c.id });
            }
        }
        self.events.push_back(Event::PeerReset { reason });
        self.ctl.clear();
        self.sent_read_notify = false;
        self.bad_streak = 0;
        self.rx_more = false;
        self.features = self.cfg.features;
        self.version_attempts = 0;
        self.link = LinkState::Down;
        // A bad index or a rewritten descriptor means the remote restarted, and whatever it wrote since is its
        // fresh start. Garbage in the stream, or a restart the caller announced, leaves only stale bytes.
        self.keep_unread = matches!(reason, ResetReason::Fault(_));
        self.recover();
    }

    /// Leaves `Down` once both rings can be re-synchronised, and proposes a version again.
    fn recover(&mut self) {
        if self.fifo.resync(&mut self.io, self.keep_unread).is_ok() {
            self.link = LinkState::Negotiating;
            self.send_version();
        }
    }
}

/// A channel name as `qcom_glink_rx_open` takes it: a C string of at most 31 bytes. `None` if it is empty, too long
/// or not UTF-8. Anything after the first NUL is ignored.
fn validate_name(raw: &[u8]) -> Option<&str> {
    let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    let name = core::str::from_utf8(&raw[..end]).ok()?;
    if name.is_empty() || name.len() >= wire::NAME_SIZE {
        return None;
    }
    Some(name)
}
