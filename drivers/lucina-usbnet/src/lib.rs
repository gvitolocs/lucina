//! The part of a USB Ethernet adapter that is the same for every chip: moving frames between the network stack
//! and the chip's driver, and watching the link.
//!
//! The kernel's USB thread owns the device and is the only one that may wait for transfers; the network stack
//! sends and receives from softirq context and must not wait. So the two meet at two queues: the stack pushes
//! frames to send into a TX queue and takes received frames from an RX queue, and the USB thread runs
//! [`Pump::pass`] on every pass, which empties the TX queue into the driver, fills the RX queue from it and polls
//! the link about once a second ([`LINK_POLL_MS`]). The queues and their locks are the kernel's (component
//! `aster-usb-net`); this crate gets them as closures, so that it is tested on the host against mock drivers.
//!
//! A chip driver implements [`EthernetBackend`]: `lucina-ax88179` for the ASIX AX88179, and the kernel's CDC-ECM/NCM
//! driver for adapters of the USB communications class, whose protocol logic is here too:
//!
//! * [`desc`] finds a CDC Ethernet function in a raw configuration descriptor: its control and data interfaces
//!   (Union functional descriptor), the Ethernet Networking functional descriptor (MAC string, maximum segment
//!   size), the notification endpoint and the data interface's bulk pair.
//! * [`ecm`] is the CDC-ECM side: class requests, notifications, the MAC string and bulk framing.
//! * [`ncm`] is the CDC-NCM side: NTB parameters and 16-bit NTBs (NTH16 + NDP16), parsing and building.
//!
//! Sources of truth for that part: the USB CDC 1.2, ECM 1.2 and NCM 1.0 specifications, and Linux
//! `drivers/net/usb/cdc_ether.c`, `usbnet.c` and `cdc_ncm.c` (GPL-2.0: read for behaviour only, nothing copied).
//!
//! `no_std` with `alloc`, no `unsafe`, no dependencies.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod desc;
pub mod ecm;
pub mod ncm;

use alloc::{string::String, vec, vec::Vec};

/// Ethernet header length.
pub const ETH_HLEN: usize = 14;
/// The usual Ethernet MTU, used when a device does not state a maximum segment size.
pub const ETH_DATA_LEN: usize = 1500;

/// How often the link is polled. The test plan expects cable changes to show within about a second.
pub const LINK_POLL_MS: u64 = 1000;
/// Frames handed to the driver per pass. Neither direction waits, so this only bounds the copies of one pass.
pub const MAX_TX_PER_PASS: usize = 256;
/// Frames taken from the driver per pass.
pub const MAX_RX_PER_PASS: usize = 256;
/// Passes in a row in which every driver call failed, after which the adapter is given up.
pub const MAX_FAILED_PASSES: u32 = 100;

/// Why a driver call failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackendError {
    /// The adapter is unplugged or no longer answers: stop using it.
    Gone,
    /// This call failed (the frame is lost); later calls may work.
    Failed,
}

/// A USB Ethernet chip driver, as the [`Pump`] uses it. All calls come from the USB thread.
pub trait EthernetBackend {
    /// The adapter's MAC address.
    fn mac_addr(&self) -> [u8; 6];

    /// Sends one Ethernet frame (destination address first, no FCS). May wait for the transfer.
    fn send(&mut self, frame: &[u8]) -> Result<(), BackendError>;

    /// Hands frames from `next` to the chip, at most `limit`, and returns how many it took: 0 when `next` has none or
    /// the chip has no room for them, which leaves them in `next` for a later pass. A chip that can take several
    /// frames in one transfer, or can say it is full without waiting, overrides this.
    fn send_frames(
        &mut self,
        next: &mut dyn FnMut() -> Option<Vec<u8>>,
        _limit: usize,
    ) -> Result<usize, BackendError> {
        let Some(frame) = next() else { return Ok(0) };
        self.send(&frame).map(|()| 1)
    }

    /// Copies the next received Ethernet frame into `out` and returns its length; `Ok(None)` when none is
    /// waiting. Must not wait. `out` is [`EthernetBackend::max_frame_len`] bytes long.
    fn receive(&mut self, out: &mut [u8]) -> Result<Option<usize>, BackendError>;

    /// The longest frame [`EthernetBackend::receive`] may return.
    fn max_frame_len(&self) -> usize {
        1518
    }

    /// Checks the link and configures the chip for it when it changed. `elapsed_ms` is the time since the
    /// previous call (0 on the first). Returns whether the link is up.
    fn poll_link(&mut self, elapsed_ms: u32) -> Result<bool, BackendError>;

    /// The current link for the log, e.g. `1000/full`.
    fn link_text(&self) -> String;

    /// The driver's counters for the log.
    fn stats_text(&self) -> String;
}

/// Something the kernel should log or act on, from one [`Pump::pass`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// The link came up; [`EthernetBackend::link_text`].
    LinkUp(String),
    /// The link went down; [`EthernetBackend::stats_text`].
    LinkDown(String),
    /// The adapter is gone (unplugged, or [`MAX_FAILED_PASSES`] failed passes): drop the function.
    Gone,
}

/// The pump's own counters (the driver has more).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    pub tx_frames: u64,
    pub rx_frames: u64,
    /// Frames to send that were dropped because the link was down.
    pub tx_dropped: u64,
    /// Passes that left a received frame waiting because the RX queue was full. Nothing is dropped: the transfers
    /// stay with the chip, whose buffer fills, which is what slows the sender down.
    pub rx_held: u64,
    pub tx_errors: u64,
    pub rx_errors: u64,
    pub link_errors: u64,
}

/// What one pass did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pass {
    pub sent: usize,
    pub received: usize,
    pub events: Vec<Event>,
}

impl Pass {
    pub fn gone(&self) -> bool {
        self.events.contains(&Event::Gone)
    }
}

/// The frame mover of one adapter; see the crate documentation.
#[derive(Debug)]
pub struct Pump {
    rx_buf: Vec<u8>,
    /// The length of a frame in `rx_buf` that the RX queue had no room for.
    rx_waiting: Option<usize>,
    link_up: bool,
    last_link_poll: Option<u64>,
    failed_passes: u32,
    gone: bool,
    counters: Counters,
}

impl Pump {
    pub fn new(backend: &dyn EthernetBackend) -> Self {
        Self {
            rx_buf: vec![0; backend.max_frame_len()],
            rx_waiting: None,
            link_up: false,
            last_link_poll: None,
            failed_passes: 0,
            gone: false,
            counters: Counters::default(),
        }
    }

    /// Whether the link was up at the last poll. The network device reports it as `can_send`.
    pub fn link_up(&self) -> bool {
        self.link_up
    }

    pub fn counters(&self) -> Counters {
        self.counters
    }

    /// One pass at time `now_ms` (any monotonic clock): the link poll when it is due, then up to
    /// [`MAX_TX_PER_PASS`] frames from `next_tx` to the driver, then up to [`MAX_RX_PER_PASS`] frames from the
    /// driver to `deliver`, which returns `false` when it has no room: that frame waits for the next pass, and no
    /// more is taken from the driver until then.
    ///
    /// While the link is down, frames to send are dropped (the network device refuses new ones then). After an
    /// [`Event::Gone`] the pump does nothing more.
    pub fn pass(
        &mut self,
        backend: &mut dyn EthernetBackend,
        now_ms: u64,
        next_tx: &mut dyn FnMut() -> Option<Vec<u8>>,
        deliver: &mut dyn FnMut(&[u8]) -> bool,
    ) -> Pass {
        let mut pass = Pass::default();
        if self.gone {
            return pass;
        }
        let mut calls = 0u32;
        let mut failures = 0u32;

        let due = self
            .last_link_poll
            .is_none_or(|last| now_ms.saturating_sub(last) >= LINK_POLL_MS);
        if due {
            let elapsed = self.last_link_poll.map_or(0, |last| {
                now_ms.saturating_sub(last).min(u32::MAX as u64) as u32
            });
            self.last_link_poll = Some(now_ms);
            calls += 1;
            match backend.poll_link(elapsed) {
                Ok(up) if up != self.link_up => {
                    self.link_up = up;
                    pass.events.push(if up {
                        Event::LinkUp(backend.link_text())
                    } else {
                        Event::LinkDown(backend.stats_text())
                    });
                }
                Ok(_) => {}
                Err(BackendError::Gone) => return self.give_up(pass),
                Err(BackendError::Failed) => {
                    failures += 1;
                    self.counters.link_errors += 1;
                }
            }
        }

        let mut tx_left = MAX_TX_PER_PASS;
        while tx_left > 0 {
            if !self.link_up {
                let Some(_frame) = next_tx() else { break };
                self.counters.tx_dropped += 1;
                tx_left -= 1;
                continue;
            }
            match backend.send_frames(next_tx, tx_left) {
                Ok(0) => break,
                Ok(sent) => {
                    calls += 1;
                    pass.sent += sent;
                    self.counters.tx_frames += sent as u64;
                    tx_left = tx_left.saturating_sub(sent);
                }
                Err(BackendError::Gone) => return self.give_up(pass),
                Err(BackendError::Failed) => {
                    calls += 1;
                    failures += 1;
                    self.counters.tx_errors += 1;
                    tx_left -= 1;
                }
            }
        }

        let mut rx_room = true;
        if let Some(len) = self.rx_waiting.take() {
            if deliver(&self.rx_buf[..len]) {
                pass.received += 1;
                self.counters.rx_frames += 1;
            } else {
                self.rx_waiting = Some(len);
                self.counters.rx_held += 1;
                rx_room = false;
            }
        }
        for _ in 0..MAX_RX_PER_PASS {
            if !rx_room {
                break;
            }
            calls += 1;
            match backend.receive(&mut self.rx_buf) {
                Ok(Some(len)) => {
                    let len = len.min(self.rx_buf.len());
                    if deliver(&self.rx_buf[..len]) {
                        pass.received += 1;
                        self.counters.rx_frames += 1;
                    } else {
                        self.rx_waiting = Some(len);
                        self.counters.rx_held += 1;
                        rx_room = false;
                    }
                }
                Ok(None) => break,
                Err(BackendError::Gone) => return self.give_up(pass),
                Err(BackendError::Failed) => {
                    failures += 1;
                    self.counters.rx_errors += 1;
                    break;
                }
            }
        }

        if calls > 0 && failures == calls {
            self.failed_passes += 1;
            if self.failed_passes >= MAX_FAILED_PASSES {
                return self.give_up(pass);
            }
        } else {
            self.failed_passes = 0;
        }
        pass
    }

    fn give_up(&mut self, mut pass: Pass) -> Pass {
        self.gone = true;
        self.link_up = false;
        pass.events.push(Event::Gone);
        pass
    }
}

/// `02:00:5e:00:00:02`
pub fn format_mac(mac: [u8; 6]) -> String {
    use core::fmt::Write;
    let mut text = String::with_capacity(17);
    for (i, byte) in mac.iter().enumerate() {
        let _ = write!(text, "{}{:02x}", if i == 0 { "" } else { ":" }, byte);
    }
    text
}

#[cfg(test)]
mod tests;
