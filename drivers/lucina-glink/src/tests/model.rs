//! Two sides over one RAM array: the code under test as the AP, and a small model of the modem.
//!
//! The model is written from the modem's chair and does not use `wire.rs`: it has its own frame builders, its own
//! table of how long each command is, and its own parser. It does share [`Fifo`] with the code under test, with
//! the roles swapped ([`Side::Remote`]) as the task asked; the ring arithmetic is checked against hand-computed
//! memory images in `fifo.rs` so a mistake there cannot cancel out.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::string::String;
use std::vec;
use std::vec::Vec;

use super::support::{Port, Ram, ap_layout, remote_layout};
use crate::fifo::{Fifo, Side};
use crate::{
    ChannelId, ChannelState, Config, Event, Fault, Glink, LinkState, OpenFailure, ResetReason,
    SendFailure,
};

// ---- the modem ------------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Msg {
    Version(u16, u32),
    VersionAck(u16, u32),
    Open(u16, String),
    OpenAck(u16),
    Close(u16),
    CloseAck(u16),
    Intents(u16, Vec<(u32, u32)>),
    RxDone {
        cid: u16,
        iid: u32,
        reuse: bool,
    },
    IntentReq {
        cid: u16,
        size: u32,
    },
    IntentReqAck {
        cid: u16,
        granted: u32,
    },
    Data {
        cont: bool,
        cid: u16,
        iid: u32,
        chunk: u32,
        left: u32,
        payload: Vec<u8>,
    },
    Signals(u16, u32),
    ReadNotif,
    Unknown(u16),
}

fn le16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn hdr(cmd: u16, p1: u16, p2: u32) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&cmd.to_le_bytes());
    v.extend_from_slice(&p1.to_le_bytes());
    v.extend_from_slice(&p2.to_le_bytes());
    v
}

pub fn m_version(v: u16, f: u32) -> Vec<u8> {
    hdr(0, v, f)
}
pub fn m_version_ack(v: u16, f: u32) -> Vec<u8> {
    hdr(1, v, f)
}
pub fn m_open(id: u16, name: &str) -> Vec<u8> {
    let mut v = hdr(2, id, name.len() as u32 + 1);
    v.extend_from_slice(name.as_bytes());
    v.push(0);
    v
}
pub fn m_open_ack(id: u16) -> Vec<u8> {
    hdr(4, id, 0)
}
pub fn m_close(id: u16) -> Vec<u8> {
    hdr(3, id, 0)
}
pub fn m_close_ack(id: u16) -> Vec<u8> {
    hdr(11, id, 0)
}
pub fn m_intent(id: u16, size: u32, iid: u32) -> Vec<u8> {
    let mut v = hdr(5, id, 1);
    v.extend_from_slice(&size.to_le_bytes());
    v.extend_from_slice(&iid.to_le_bytes());
    v
}
pub fn m_rx_done(id: u16, iid: u32, reuse: bool) -> Vec<u8> {
    hdr(if reuse { 14 } else { 6 }, id, iid)
}
pub fn m_intent_req(id: u16, size: u32) -> Vec<u8> {
    hdr(7, id, size)
}
pub fn m_intent_req_ack(id: u16, granted: u32) -> Vec<u8> {
    hdr(8, id, granted)
}
pub fn m_data(cont: bool, id: u16, iid: u32, payload: &[u8], left: u32) -> Vec<u8> {
    let mut v = hdr(if cont { 12 } else { 9 }, id, iid);
    v.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    v.extend_from_slice(&left.to_le_bytes());
    v.extend_from_slice(payload);
    v
}
pub fn m_signals(id: u16, sigs: u32) -> Vec<u8> {
    hdr(15, id, sigs)
}
pub fn m_read_notif() -> Vec<u8> {
    hdr(13, 0, 0)
}

pub struct Modem {
    pub port: Port,
    pub fifo: Fifo,
    pub seen: Vec<Msg>,
    /// The exact padded bytes of each message in `seen`.
    pub raw: Vec<Vec<u8>>,
    pub taken: usize,
    backlog: VecDeque<Vec<u8>>,
}

impl Modem {
    pub fn new(ram: &Rc<RefCell<Ram>>) -> Modem {
        let mut port = Port::new(ram, Side::Remote);
        let layout = remote_layout(ram);
        let fifo = Fifo::new(&mut port, layout).unwrap();
        Modem {
            port,
            fifo,
            seen: Vec::new(),
            raw: Vec::new(),
            taken: 0,
            backlog: VecDeque::new(),
        }
    }

    /// Reads everything the AP has written, one message at a time, by the modem's own length table.
    pub fn pump_rx(&mut self) {
        loop {
            let avail = self.fifo.rx_avail(&mut self.port).unwrap();
            if avail < 8 {
                return;
            }
            let mut h = [0u8; 8];
            self.fifo.rx_peek(&mut self.port, 0, &mut h);
            let (cmd, p1, p2) = (le16(&h, 0), le16(&h, 2), le32(&h, 4));
            let size = match cmd {
                2 => 8 + (p2 & 0xffff) as usize,
                5 => 8 + 8 * p2 as usize,
                9 | 12 => {
                    if avail < 16 {
                        return;
                    }
                    let mut d = [0u8; 16];
                    self.fifo.rx_peek(&mut self.port, 0, &mut d);
                    16 + le32(&d, 8) as usize
                }
                _ => 8,
            };
            if avail < size {
                return;
            }
            let padded = (size + 7) & !7;
            let mut f = vec![0u8; padded];
            self.fifo.rx_peek(&mut self.port, 0, &mut f);
            let msg = match cmd {
                0 => Msg::Version(p1, p2),
                1 => Msg::VersionAck(p1, p2),
                2 => {
                    let n = (p2 & 0xffff) as usize;
                    assert_eq!(f[8 + n - 1], 0, "name not NUL terminated");
                    Msg::Open(p1, String::from_utf8(f[8..8 + n - 1].to_vec()).unwrap())
                }
                3 => Msg::Close(p1),
                4 => Msg::OpenAck(p1),
                5 => Msg::Intents(
                    p1,
                    (0..p2 as usize)
                        .map(|i| (le32(&f, 8 + 8 * i), le32(&f, 12 + 8 * i)))
                        .collect(),
                ),
                6 | 14 => Msg::RxDone {
                    cid: p1,
                    iid: p2,
                    reuse: cmd == 14,
                },
                7 => Msg::IntentReq { cid: p1, size: p2 },
                8 => Msg::IntentReqAck {
                    cid: p1,
                    granted: p2,
                },
                9 | 12 => Msg::Data {
                    cont: cmd == 12,
                    cid: p1,
                    iid: p2,
                    chunk: le32(&f, 8),
                    left: le32(&f, 12),
                    payload: f[16..size].to_vec(),
                },
                11 => Msg::CloseAck(p1),
                13 => Msg::ReadNotif,
                15 => Msg::Signals(p1, p2),
                other => Msg::Unknown(other),
            };
            self.fifo.rx_advance(&mut self.port, padded);
            self.seen.push(msg);
            self.raw.push(f);
        }
    }

    /// Queues a message for the AP and writes what fits, ringing the AP's bell when anything went out.
    pub fn send(&mut self, frame: Vec<u8>) {
        self.backlog.push_back(frame);
        self.retry();
    }

    pub fn retry(&mut self) {
        let mut wrote = false;
        while let Some(f) = self.backlog.front() {
            if self.fifo.tx_write(&mut self.port, &[f], false).unwrap() {
                self.backlog.pop_front();
                wrote = true;
            } else {
                break;
            }
        }
        if wrote {
            use crate::fifo::Doorbell;
            self.port.ring();
        }
    }

    pub fn idle(&self) -> bool {
        self.backlog.is_empty()
    }

    /// Messages that arrived since the last call.
    pub fn take(&mut self) -> Vec<Msg> {
        let v = self.seen[self.taken..].to_vec();
        self.taken = self.seen.len();
        v
    }
}

// ---- the rig --------------------------------------------------------------------------------------------------

pub struct Rig {
    pub ram: Rc<RefCell<Ram>>,
    pub ap: Glink<Port>,
    pub modem: Modem,
}

impl Rig {
    pub fn new(tx_len: usize, rx_len: usize) -> Rig {
        Rig::with_config(tx_len, rx_len, Config::default())
    }

    pub fn with_config(tx_len: usize, rx_len: usize, cfg: Config) -> Rig {
        let ram = Ram::new(tx_len, rx_len);
        let ap = Glink::with_config(Port::new(&ram, Side::Ap), ap_layout(&ram, true), cfg).unwrap();
        let modem = Modem::new(&ram);
        Rig { ram, ap, modem }
    }

    /// Lets both sides run until nothing moves: the AP's interrupt, the modem draining and retrying.
    pub fn settle(&mut self) -> Vec<Event> {
        let mut ev = Vec::new();
        for _ in 0..16 {
            ev.extend(self.ap.poll());
            self.modem.pump_rx();
            self.modem.retry();
        }
        ev
    }

    pub fn modem_says(&mut self, frames: Vec<Vec<u8>>) -> Vec<Event> {
        for f in frames {
            self.modem.send(f);
        }
        self.settle()
    }
}

pub struct Up {
    pub ch: ChannelId,
    /// The AP's id for the channel (its lcid), which the modem addresses.
    pub ap_lcid: u16,
    /// The modem's id for it, which the AP addresses.
    pub m_cid: u16,
    /// The AP's default intents as the modem saw them: (size, iid).
    pub intents: Vec<(u32, u32)>,
}

/// Version negotiation and the remote-initiated open of `IPCRTR`, the way the modem does it.
pub fn bring_up(rig: &mut Rig) -> Up {
    let ev = rig.settle();
    assert!(ev.is_empty());
    assert_eq!(rig.modem.take(), vec![Msg::Version(1, 1)]);
    let ev = rig.modem_says(vec![m_version_ack(1, 1), m_version(1, 1)]);
    assert_eq!(ev, vec![Event::LinkUp { features: 1 }]);
    assert_eq!(rig.modem.take(), vec![Msg::VersionAck(1, 1)]);
    let m_cid = 5;
    let ev = rig.modem_says(vec![m_open(m_cid, "IPCRTR")]);
    assert!(
        ev.is_empty(),
        "not open until the modem acks our OPEN: {ev:?}"
    );
    let seen = rig.modem.take();
    assert_eq!(seen[0], Msg::OpenAck(m_cid));
    let Msg::Open(ap_lcid, name) = seen[1].clone() else {
        panic!("{seen:?}")
    };
    assert_eq!(name, "IPCRTR");
    assert_eq!(seen.len(), 2);
    let ev = rig.modem_says(vec![m_open_ack(ap_lcid)]);
    let [Event::ChannelOpened { channel, name }] = ev.as_slice() else {
        panic!("{ev:?}")
    };
    assert_eq!(name, "IPCRTR");
    let ch = *channel;
    let mut intents = Vec::new();
    for m in rig.modem.take() {
        let Msg::Intents(cid, v) = m else {
            panic!("{m:?}")
        };
        assert_eq!(cid, ap_lcid);
        assert_eq!(v.len(), 1);
        intents.push(v[0]);
    }
    assert_eq!(rig.ap.channel_state(ch), Some(ChannelState::Open));
    Up {
        ch,
        ap_lcid,
        m_cid,
        intents,
    }
}

fn pattern(n: usize, seed: u8) -> Vec<u8> {
    (0..n)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

// ---- version negotiation ----------------------------------------------------------------------------------------

#[test]
fn negotiation_matching_features() {
    let mut rig = Rig::new(1024, 1024);
    rig.settle();
    // `qcom_glink_native_probe` sends VERSION(1, INTENT_REUSE) first thing.
    assert_eq!(rig.modem.take(), vec![Msg::Version(1, 1)]);
    assert_eq!(rig.ap.link_state(), LinkState::Negotiating);
    let ev = rig.modem_says(vec![m_version_ack(1, 1)]);
    assert_eq!(ev, vec![Event::LinkUp { features: 1 }]);
    assert_eq!(rig.ap.link_state(), LinkState::Up);
    // The modem's own proposal is acked with the common features.
    rig.modem_says(vec![m_version(1, 1)]);
    assert_eq!(rig.modem.take(), vec![Msg::VersionAck(1, 1)]);
    assert!(rig.ram.borrow().bells_to_remote > 0);
}

#[test]
fn negotiation_remote_proposes_fewer_features() {
    let mut rig = Rig::new(1024, 1024);
    rig.settle();
    rig.modem.take();
    // The modem offers no reuse: we narrow to 0 and ack with 0; its ack of our VERSION then carries 0 too.
    rig.modem_says(vec![m_version(1, 0)]);
    assert_eq!(rig.modem.take(), vec![Msg::VersionAck(1, 0)]);
    assert_eq!(rig.ap.features(), 0);
    let ev = rig.modem_says(vec![m_version_ack(1, 0)]);
    assert_eq!(ev, vec![Event::LinkUp { features: 0 }]);
}

#[test]
fn negotiation_downgrade_by_ack() {
    let mut rig = Rig::new(1024, 1024);
    rig.settle();
    rig.modem.take();
    // An ack with fewer features than we offered: no LinkUp, we propose again with the narrower set.
    let ev = rig.modem_says(vec![m_version_ack(1, 0)]);
    assert!(ev.is_empty());
    assert_eq!(rig.modem.take(), vec![Msg::Version(1, 0)]);
    assert_eq!(rig.ap.link_state(), LinkState::Negotiating);
    let ev = rig.modem_says(vec![m_version_ack(1, 0)]);
    assert_eq!(ev, vec![Event::LinkUp { features: 0 }]);
}

#[test]
fn negotiation_downgrade_from_newer_version() {
    let mut rig = Rig::new(1024, 1024);
    rig.settle();
    rig.modem.take();
    // A remote proposing version 2 gets version 1 back with our features unchanged (the `default:` arm).
    rig.modem_says(vec![m_version(2, 0xff)]);
    assert_eq!(rig.modem.take(), vec![Msg::VersionAck(1, 1)]);
    assert_eq!(rig.ap.features(), 1);
    // A remote that acks in version 2 makes us propose version 1 again.
    rig.modem_says(vec![m_version_ack(2, 1)]);
    assert_eq!(rig.modem.take(), vec![Msg::Version(1, 1)]);
}

#[test]
fn negotiation_version_zero_ack_rejects() {
    let mut rig = Rig::new(1024, 1024);
    rig.settle();
    rig.modem.take();
    let ev = rig.modem_says(vec![m_version_ack(0, 0)]);
    assert_eq!(ev, vec![Event::LinkRejected]);
    assert_eq!(rig.ap.link_state(), LinkState::Rejected);
    // A version-0 VERSION from the remote is ignored: no ack.
    rig.modem_says(vec![m_version(0, 0)]);
    assert!(rig.modem.take().is_empty());
}

#[test]
fn negotiation_endless_refusal_is_bounded() {
    let mut rig = Rig::new(1024, 1024);
    rig.settle();
    let mut rejected = 0;
    for _ in 0..30 {
        let ev = rig.modem_says(vec![m_version_ack(2, 1)]);
        rejected += ev.iter().filter(|e| **e == Event::LinkRejected).count();
    }
    let versions = rig
        .modem
        .take()
        .iter()
        .filter(|m| matches!(m, Msg::Version(..)))
        .count();
    assert_eq!(rejected, 1);
    assert_eq!(versions, 8);
    assert_eq!(rig.ap.link_state(), LinkState::Rejected);
}

#[test]
fn downgrade_without_reuse_means_one_shot_intents() {
    let mut rig = Rig::new(1024, 4096);
    rig.settle();
    rig.modem.take();
    rig.modem_says(vec![m_version(1, 0), m_version_ack(1, 0)]);
    rig.modem.take();
    rig.modem_says(vec![m_open(5, "IPCRTR")]);
    let seen = rig.modem.take();
    let Msg::Open(lcid, _) = seen[1].clone() else {
        panic!()
    };
    let ev = rig.modem_says(vec![m_open_ack(lcid)]);
    assert!(matches!(ev[0], Event::ChannelOpened { .. }));
    let iid = rig
        .modem
        .take()
        .iter()
        .find_map(|m| {
            if let Msg::Intents(_, v) = m {
                Some(v[0].1)
            } else {
                None
            }
        })
        .unwrap();
    // Data into a one-shot intent is answered with plain RX_DONE (6), not RX_DONE_W_REUSE (14).
    let ev = rig.modem_says(vec![m_data(false, 5, iid, b"hi", 0)]);
    assert!(matches!(&ev[0], Event::Data { data, .. } if data == b"hi"));
    assert_eq!(
        rig.modem.take(),
        vec![Msg::RxDone {
            cid: lcid,
            iid,
            reuse: false
        }]
    );
}

// ---- opening ----------------------------------------------------------------------------------------------------

#[test]
fn remote_opens_ipcrtr_and_we_follow_linux_order() {
    let mut rig = Rig::new(1024, 1024);
    let up = bring_up(&mut rig);
    // Five 1 KiB intents with distinct ids (`qcom_glink_announce_create`'s default), one INTENT message each.
    assert_eq!(up.intents.len(), 5);
    assert!(up.intents.iter().all(|&(size, _)| size == 1024));
    let mut ids: Vec<u32> = up.intents.iter().map(|i| i.1).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), 5);
    assert_eq!(up.ap_lcid, 1);
}

#[test]
fn remote_open_of_an_unbound_name_is_offered_and_not_acked() {
    let mut rig = Rig::new(1024, 1024);
    rig.settle();
    rig.modem.take();
    let ev = rig.modem_says(vec![m_open(9, "DATA")]);
    let [Event::Offered { channel, name }] = ev.as_slice() else {
        panic!("{ev:?}")
    };
    assert_eq!(name, "DATA");
    // Like an rpmsg device nobody binds: no OPEN_ACK, no OPEN.
    assert!(rig.modem.take().is_empty());
    assert_eq!(rig.ap.channel_state(*channel), Some(ChannelState::Offered));
    // Opening the same name now is the remote-initiated order.
    let id = rig.ap.open("DATA").unwrap();
    assert_eq!(id, *channel);
    rig.settle();
    let seen = rig.modem.take();
    assert_eq!(seen[0], Msg::OpenAck(9));
    assert!(matches!(&seen[1], Msg::Open(_, n) if n == "DATA"));
}

#[test]
fn local_open_waits_for_both_acks_then_acks_the_remote_open() {
    let mut rig = Rig::new(1024, 1024);
    rig.settle();
    rig.modem.take();
    let ch = rig.ap.open("QMI_CTL").unwrap();
    assert_eq!(rig.ap.channel_state(ch), Some(ChannelState::Opening));
    rig.settle();
    let seen = rig.modem.take();
    let Msg::Open(lcid, name) = seen[0].clone() else {
        panic!("{seen:?}")
    };
    assert_eq!(name, "QMI_CTL");
    // Order 1: OPEN_ACK first, then the remote's own OPEN, then we ack it.
    assert!(rig.modem_says(vec![m_open_ack(lcid)]).is_empty());
    assert!(rig.modem.take().is_empty());
    let ev = rig.modem_says(vec![m_open(3, "QMI_CTL")]);
    assert!(matches!(&ev[..], [Event::ChannelOpened { channel, .. }] if *channel == ch));
    assert_eq!(rig.modem.take()[0], Msg::OpenAck(3));

    // Order 2: the remote's OPEN overtakes its OPEN_ACK; the ack still waits for both.
    let ch2 = rig.ap.open("QDSS").unwrap();
    rig.settle();
    let Msg::Open(lcid2, _) = rig.modem.take()[0].clone() else {
        panic!()
    };
    assert!(rig.modem_says(vec![m_open(4, "QDSS")]).is_empty());
    assert!(rig.modem.take().is_empty());
    let ev = rig.modem_says(vec![m_open_ack(lcid2)]);
    assert!(matches!(&ev[..], [Event::ChannelOpened { channel, .. }] if *channel == ch2));
    assert_eq!(rig.modem.take()[0], Msg::OpenAck(4));
}

#[test]
fn local_open_times_out_after_five_seconds_and_closes() {
    let mut rig = Rig::new(1024, 1024);
    rig.ap.tick(1000);
    rig.settle();
    rig.modem.take();
    let ch = rig.ap.open("NOBODY").unwrap();
    rig.settle();
    let Msg::Open(lcid, _) = rig.modem.take()[0].clone() else {
        panic!()
    };
    assert_eq!(rig.ap.next_deadline(), Some(6000));
    assert!(rig.ap.tick(5999).is_empty());
    let ev = rig.ap.tick(6000);
    assert_eq!(
        ev,
        vec![Event::OpenFailed {
            channel: ch,
            reason: OpenFailure::Timeout
        }]
    );
    rig.settle();
    assert_eq!(rig.modem.take(), vec![Msg::Close(lcid)]);
    // A late OPEN_ACK for a channel being closed does not open it.
    assert!(rig.modem_says(vec![m_open_ack(lcid)]).is_empty());
    let ev = rig.modem_says(vec![m_close_ack(lcid)]);
    assert!(ev.is_empty(), "already reported: {ev:?}");
    assert_eq!(rig.ap.channel_state(ch), None);
}

#[test]
fn remote_open_without_our_ack_times_out_with_close() {
    let mut rig = Rig::new(1024, 1024);
    rig.ap.tick(0);
    rig.settle();
    rig.modem.take();
    rig.modem_says(vec![m_open(7, "IPCRTR")]);
    let seen = rig.modem.take();
    let Msg::Open(lcid, _) = seen[1].clone() else {
        panic!()
    };
    // `qcom_glink_create_remote`: no OPEN_ACK within 5 s -> CLOSE to undo.
    let ev = rig.ap.tick(5000);
    assert!(matches!(&ev[..], [Event::OpenFailed { .. }]));
    rig.settle();
    assert_eq!(rig.modem.take(), vec![Msg::Close(lcid)]);
}

// ---- sending ----------------------------------------------------------------------------------------------------

#[test]
fn send_picks_the_smallest_free_intent_exact_fit_first() {
    let mut rig = Rig::new(4096, 4096);
    let up = bring_up(&mut rig);
    rig.modem_says(vec![
        m_intent(up.m_cid, 100, 1),
        m_intent(up.m_cid, 50, 2),
        m_intent(up.m_cid, 60, 3),
    ]);
    let mut iids = Vec::new();
    for (len, expect_iid) in [(40usize, 2u32), (60, 3), (90, 1)] {
        rig.ap.send(up.ch, &pattern(len, 1)).unwrap();
        rig.settle();
        let seen = rig.modem.take();
        let [
            Msg::Data {
                iid,
                chunk,
                left,
                cid,
                cont,
                ..
            },
        ] = seen.as_slice()
        else {
            panic!("{seen:?}")
        };
        assert_eq!(
            (*iid, *chunk as usize, *left, *cid, *cont),
            (expect_iid, len, 0, up.ap_lcid, false)
        );
        iids.push(*iid);
    }
    // All three are in use now: the next send has to ask for an intent of its size.
    rig.ap.send(up.ch, &pattern(10, 1)).unwrap();
    rig.settle();
    assert_eq!(
        rig.modem.take(),
        vec![Msg::IntentReq {
            cid: up.ap_lcid,
            size: 10
        }]
    );
}

#[test]
fn intent_request_granted_then_data_flows() {
    let mut rig = Rig::new(4096, 4096);
    let up = bring_up(&mut rig);
    rig.ap.send(up.ch, &pattern(300, 3)).unwrap();
    rig.settle();
    assert_eq!(
        rig.modem.take(),
        vec![Msg::IntentReq {
            cid: up.ap_lcid,
            size: 300
        }]
    );
    // The ack arrives before the intent: still waiting.
    rig.modem_says(vec![m_intent_req_ack(up.m_cid, 1)]);
    assert!(rig.modem.take().is_empty());
    rig.modem_says(vec![m_intent(up.m_cid, 512, 21)]);
    let seen = rig.modem.take();
    let [
        Msg::Data {
            iid: 21,
            payload,
            left: 0,
            ..
        },
    ] = seen.as_slice()
    else {
        panic!("{seen:?}")
    };
    assert_eq!(*payload, pattern(300, 3));
}

#[test]
fn intent_request_denied_fails_the_message() {
    let mut rig = Rig::new(4096, 4096);
    let up = bring_up(&mut rig);
    rig.ap.send(up.ch, &pattern(300, 3)).unwrap();
    rig.settle();
    rig.modem.take();
    let ev = rig.modem_says(vec![m_intent_req_ack(up.m_cid, 0)]);
    assert_eq!(
        ev,
        vec![Event::SendFailed {
            channel: up.ch,
            len: 300,
            reason: SendFailure::IntentDenied
        }]
    );
    assert_eq!(rig.ap.tx_backlog(), 0);
    // The channel still works afterwards.
    rig.modem_says(vec![m_intent(up.m_cid, 64, 5)]);
    rig.ap.send(up.ch, b"hello").unwrap();
    rig.settle();
    assert!(matches!(&rig.modem.take()[..], [Msg::Data { iid: 5, .. }]));
}

#[test]
fn intent_request_times_out_after_ten_seconds() {
    let mut rig = Rig::new(4096, 4096);
    rig.ap.tick(1000);
    let up = bring_up(&mut rig);
    rig.ap.send(up.ch, &pattern(300, 3)).unwrap();
    rig.settle();
    rig.modem.take();
    assert!(rig.ap.next_deadline().is_some());
    assert!(rig.ap.tick(10_999).is_empty());
    let ev = rig.ap.tick(11_000);
    assert_eq!(
        ev,
        vec![Event::SendFailed {
            channel: up.ch,
            len: 300,
            reason: SendFailure::Timeout
        }]
    );
}

#[test]
fn intents_too_small_give_up_after_four_requests() {
    let mut rig = Rig::new(4096, 4096);
    let up = bring_up(&mut rig);
    rig.ap.send(up.ch, &pattern(300, 3)).unwrap();
    let mut requests = 0;
    let mut failed = None;
    for round in 0..10 {
        rig.settle();
        requests += rig
            .modem
            .take()
            .iter()
            .filter(|m| matches!(m, Msg::IntentReq { .. }))
            .count();
        // Grant, but with an intent that is too small.
        let ev = rig.modem_says(vec![
            m_intent(up.m_cid, 8, 100 + round),
            m_intent_req_ack(up.m_cid, 1),
        ]);
        failed = failed.or_else(|| {
            ev.into_iter()
                .find(|e| matches!(e, Event::SendFailed { .. }))
        });
    }
    requests += rig
        .modem
        .take()
        .iter()
        .filter(|m| matches!(m, Msg::IntentReq { .. }))
        .count();
    assert_eq!(requests, 4);
    assert_eq!(
        failed,
        Some(Event::SendFailed {
            channel: up.ch,
            len: 300,
            reason: SendFailure::IntentTooSmall
        })
    );
}

#[test]
fn rx_done_with_reuse_keeps_the_intent_and_without_removes_it() {
    let mut rig = Rig::new(4096, 4096);
    let up = bring_up(&mut rig);
    rig.modem_says(vec![m_intent(up.m_cid, 64, 1), m_intent(up.m_cid, 64, 2)]);
    rig.ap.send(up.ch, b"A").unwrap();
    rig.ap.send(up.ch, b"B").unwrap();
    rig.settle();
    let seen = rig.modem.take();
    assert!(matches!(&seen[0], Msg::Data { iid: 1, .. }));
    assert!(matches!(&seen[1], Msg::Data { iid: 2, .. }));
    // Intent 1 comes back reusable, intent 2 is consumed.
    rig.modem_says(vec![
        m_rx_done(up.m_cid, 1, true),
        m_rx_done(up.m_cid, 2, false),
    ]);
    rig.ap.send(up.ch, b"C").unwrap();
    rig.settle();
    assert!(
        matches!(&rig.modem.take()[..], [Msg::Data { iid: 1, .. }]),
        "reused id 1, no request"
    );
    rig.ap.send(up.ch, b"D").unwrap();
    rig.settle();
    // 1 is in use again and 2 is gone: a request for an intent.
    assert_eq!(
        rig.modem.take(),
        vec![Msg::IntentReq {
            cid: up.ap_lcid,
            size: 1
        }]
    );
}

#[test]
fn multi_fragment_send_across_a_ring_wrap() {
    // 256-byte rings: chunks are limited to 224 bytes, a 1000-byte message is five chunks, and the ring wraps.
    let mut rig = Rig::new(256, 4096);
    let up = bring_up(&mut rig);
    let msg = pattern(1000, 9);
    rig.modem_says(vec![m_intent(up.m_cid, 2048, 77)]);
    rig.ap.send(up.ch, &msg).unwrap();
    // Nothing reads the ring for a while: the writer stalls and sends one READ_NOTIF into the reserve.
    rig.ap.poll();
    rig.ap.poll();
    let mut got = Vec::new();
    let mut shape = Vec::new();
    for _ in 0..50 {
        rig.settle();
        for m in rig.modem.take() {
            match m {
                Msg::Data {
                    cont,
                    iid,
                    payload,
                    left,
                    chunk,
                    cid,
                } => {
                    assert_eq!((iid, cid, chunk as usize), (77, up.ap_lcid, payload.len()));
                    shape.push((cont, payload.len(), left));
                    got.extend(payload);
                }
                Msg::ReadNotif => shape.push((false, 0, u32::MAX)),
                other => panic!("{other:?}"),
            }
        }
    }
    assert_eq!(got, msg);
    let notifs = shape.iter().filter(|s| s.2 == u32::MAX).count();
    assert!(notifs >= 1, "a full ring is announced with READ_NOTIF");
    let data: Vec<_> = shape.iter().filter(|s| s.2 != u32::MAX).collect();
    assert_eq!(data[0], &(false, 224, 776));
    assert!(
        data[1..].iter().all(|s| s.0),
        "later chunks are TX_DATA_CONT"
    );
    assert_eq!(data.last().unwrap().2, 0);
    // More than one ring's worth went through, so the head wrapped.
    assert!(got.len() > 256);
}

#[test]
fn big_message_uses_8k_chunks_on_a_big_ring() {
    let mut rig = Rig::new(16384, 4096);
    let up = bring_up(&mut rig);
    rig.modem_says(vec![m_intent(up.m_cid, 20_000, 3)]);
    let msg = pattern(20_000, 4);
    rig.ap.send(up.ch, &msg).unwrap();
    let mut shapes = Vec::new();
    let mut got = Vec::new();
    for _ in 0..20 {
        rig.settle();
        for m in rig.modem.take() {
            if let Msg::Data {
                payload,
                left,
                cont,
                ..
            } = m
            {
                shapes.push((cont, payload.len(), left));
                got.extend(payload);
            }
        }
    }
    assert_eq!(got, msg);
    assert_eq!(
        shapes,
        vec![(false, 8192, 11808), (true, 8192, 3616), (true, 3616, 0)]
    );
}

#[test]
fn send_errors() {
    let mut rig = Rig::new(1024, 1024);
    let up = bring_up(&mut rig);
    assert_eq!(rig.ap.send(up.ch, &[]), Err(crate::Error::InvalidLength));
    assert_eq!(
        rig.ap.send(ChannelId(999), b"x"),
        Err(crate::Error::UnknownChannel)
    );
    assert_eq!(
        rig.ap.send(up.ch, &vec![0; 300 * 1024]),
        Err(crate::Error::InvalidLength)
    );
    let cfg = Config {
        tx_queue_limit: 100,
        ..Config::default()
    };
    let mut rig = Rig::with_config(1024, 1024, cfg);
    let up = bring_up(&mut rig);
    rig.ap.send(up.ch, &[1; 60]).unwrap();
    assert_eq!(rig.ap.send(up.ch, &[1; 60]), Err(crate::Error::QueueFull));
    assert_eq!(
        rig.ap
            .open("a name that is much too long to be a glink channel"),
        Err(crate::Error::InvalidName)
    );
    assert_eq!(rig.ap.open(""), Err(crate::Error::InvalidName));
    assert_eq!(rig.ap.open("IPCRTR"), Err(crate::Error::AlreadyOpen));
}

// ---- receiving --------------------------------------------------------------------------------------------------

#[test]
fn receive_fragments_across_a_wrap_and_release_the_intent() {
    // The modem's ring is 512 bytes; three fragments of 400+400+200 bytes cannot be in it at once.
    let mut rig = Rig::new(1024, 512);
    let up = bring_up(&mut rig);
    let (_, iid) = up.intents[0];
    let msg = pattern(1000, 5);
    let ev = rig.modem_says(vec![
        m_data(false, up.m_cid, iid, &msg[..400], 600),
        m_data(true, up.m_cid, iid, &msg[400..800], 200),
        m_data(true, up.m_cid, iid, &msg[800..], 0),
    ]);
    assert_eq!(
        ev,
        vec![Event::Data {
            channel: up.ch,
            data: msg.clone()
        }]
    );
    // One RX_DONE_W_REUSE for the intent, carrying the AP's lcid and the intent id.
    assert_eq!(
        rig.modem.take(),
        vec![Msg::RxDone {
            cid: up.ap_lcid,
            iid,
            reuse: true
        }]
    );
    // The intent is reusable: a second message goes into it.
    let ev = rig.modem_says(vec![m_data(false, up.m_cid, iid, b"again", 0)]);
    assert_eq!(
        ev,
        vec![Event::Data {
            channel: up.ch,
            data: b"again".to_vec()
        }]
    );
}

#[test]
fn receive_overflow_and_stray_fragments_are_dropped() {
    let mut rig = Rig::new(1024, 8192);
    let up = bring_up(&mut rig);
    let (_, iid) = up.intents[0];
    // 1100 bytes into a 1 KiB intent: "the packet header lied", dropped, but the intent is handed back.
    let ev = rig.modem_says(vec![m_data(false, up.m_cid, iid, &pattern(1100, 1), 0)]);
    assert!(ev.is_empty());
    assert_eq!(
        rig.modem.take(),
        vec![Msg::RxDone {
            cid: up.ap_lcid,
            iid,
            reuse: true
        }]
    );
    assert_eq!(rig.ap.violations(), 1);
    // A continuation with nothing to continue.
    let ev = rig.modem_says(vec![m_data(true, up.m_cid, iid, b"xx", 0)]);
    assert!(ev.is_empty());
    assert_eq!(rig.ap.violations(), 2);
    // Unknown intent, unknown channel: dropped, and later traffic is still handled.
    rig.modem_says(vec![
        m_data(false, up.m_cid, 9999, b"x", 0),
        m_data(false, 77, iid, b"x", 0),
    ]);
    assert_eq!(rig.ap.violations(), 4);
    let ev = rig.modem_says(vec![m_data(false, up.m_cid, iid, b"ok", 0)]);
    assert_eq!(
        ev,
        vec![Event::Data {
            channel: up.ch,
            data: b"ok".to_vec()
        }]
    );
}

#[test]
fn remote_intent_request_is_answered_with_intent_then_ack() {
    let mut rig = Rig::new(1024, 4096);
    let up = bring_up(&mut rig);
    rig.modem_says(vec![m_intent_req(up.m_cid, 3000)]);
    let seen = rig.modem.take();
    let [
        Msg::Intents(cid, v),
        Msg::IntentReqAck {
            cid: cid2,
            granted: 1,
        },
    ] = seen.as_slice()
    else {
        panic!("{seen:?}")
    };
    assert_eq!((*cid, *cid2), (up.ap_lcid, up.ap_lcid));
    assert_eq!(v[0].0, 3000);
    let iid = v[0].1;
    // A one-shot intent: after the data, RX_DONE (not W_REUSE), and the intent id is no longer accepted.
    let ev = rig.modem_says(vec![m_data(false, up.m_cid, iid, &pattern(3000, 2), 0)]);
    assert!(matches!(&ev[..], [Event::Data { data, .. }] if data.len() == 3000));
    assert_eq!(
        rig.modem.take(),
        vec![Msg::RxDone {
            cid: up.ap_lcid,
            iid,
            reuse: false
        }]
    );
    let ev = rig.modem_says(vec![m_data(false, up.m_cid, iid, b"again", 0)]);
    assert!(ev.is_empty());
    // An absurd size is denied, not allocated.
    rig.modem_says(vec![m_intent_req(up.m_cid, 0x7fff_ffff)]);
    assert_eq!(
        rig.modem.take(),
        vec![Msg::IntentReqAck {
            cid: up.ap_lcid,
            granted: 0
        }]
    );
    // A request for an unknown channel gets no answer (the C just returns).
    rig.modem_says(vec![m_intent_req(99, 10)]);
    assert!(rig.modem.take().is_empty());
}

// ---- closing, signals, notifications -----------------------------------------------------------------------------

#[test]
fn local_close_waits_for_the_ack() {
    let mut rig = Rig::new(1024, 1024);
    let up = bring_up(&mut rig);
    rig.ap.send(up.ch, b"queued, never sent").unwrap();
    rig.ap.close(up.ch).unwrap();
    let ev = rig.settle();
    assert_eq!(
        ev,
        vec![Event::SendFailed {
            channel: up.ch,
            len: 18,
            reason: SendFailure::Closed
        }]
    );
    assert!(rig.modem.take().contains(&Msg::Close(up.ap_lcid)));
    assert_eq!(rig.ap.channel_state(up.ch), Some(ChannelState::Closing));
    assert_eq!(rig.ap.send(up.ch, b"x"), Err(crate::Error::NotOpen));
    let ev = rig.modem_says(vec![m_close_ack(up.ap_lcid)]);
    assert_eq!(ev, vec![Event::ChannelClosed { channel: up.ch }]);
    // The modem has not closed its side yet, so the channel record stays until it does.
    assert_eq!(rig.ap.channel_state(up.ch), Some(ChannelState::Closing));
    rig.modem_says(vec![m_close(up.m_cid)]);
    assert_eq!(rig.modem.take(), vec![Msg::CloseAck(up.m_cid)]);
    assert_eq!(rig.ap.channel_state(up.ch), None);
}

#[test]
fn remote_close_is_answered_with_our_close_then_the_ack() {
    let mut rig = Rig::new(1024, 1024);
    let up = bring_up(&mut rig);
    let ev = rig.modem_says(vec![m_close(up.m_cid)]);
    assert_eq!(ev, vec![Event::ChannelClosed { channel: up.ch }]);
    // `qcom_glink_rx_close`: removing the rpmsg device sends our CLOSE, and only then CLOSE_ACK.
    assert_eq!(
        rig.modem.take(),
        vec![Msg::Close(up.ap_lcid), Msg::CloseAck(up.m_cid)]
    );
    let ev = rig.modem_says(vec![m_close_ack(up.ap_lcid)]);
    assert!(ev.is_empty());
    assert_eq!(rig.ap.channel_state(up.ch), None);
    // A close for an id nobody has is ignored without an ack.
    rig.modem_says(vec![m_close(42)]);
    assert!(rig.modem.take().is_empty());
    // And the name can be offered again.
    let ev = rig.modem_says(vec![m_open(6, "IPCRTR")]);
    assert!(ev.is_empty());
    assert!(matches!(
        &rig.modem.take()[..],
        [Msg::OpenAck(6), Msg::Open(..)]
    ));
}

#[test]
fn signals_both_ways() {
    let mut rig = Rig::new(1024, 1024);
    let up = bring_up(&mut rig);
    let ev = rig.modem_says(vec![
        m_signals(up.m_cid, 0x8000_0000),
        m_signals(up.m_cid, 0),
        m_signals(up.m_cid, 0x4000_0000),
    ]);
    assert_eq!(
        ev,
        vec![
            Event::Signals {
                channel: up.ch,
                signals: 0x8000_0000,
                enable: true
            },
            Event::Signals {
                channel: up.ch,
                signals: 0,
                enable: false
            },
            Event::Signals {
                channel: up.ch,
                signals: 0x4000_0000,
                enable: true
            },
        ]
    );
    rig.ap.set_flow_control(up.ch, true).unwrap();
    rig.ap.set_flow_control(up.ch, false).unwrap();
    rig.ap.send_signals(up.ch, 0x1234).unwrap();
    rig.settle();
    assert_eq!(
        rig.modem.take(),
        vec![
            Msg::Signals(up.ap_lcid, 0xc000_0000),
            Msg::Signals(up.ap_lcid, 0),
            Msg::Signals(up.ap_lcid, 0x1234)
        ]
    );
}

#[test]
fn read_notif_from_the_remote_rings_our_doorbell() {
    let mut rig = Rig::new(1024, 1024);
    rig.settle();
    let before = rig.ram.borrow().bells_to_remote;
    rig.modem_says(vec![m_read_notif()]);
    assert_eq!(rig.ram.borrow().bells_to_remote, before + 1);
    assert!(rig.modem.take().iter().all(|m| *m == Msg::Version(1, 1)));
}

#[test]
fn unknown_commands_are_skipped_and_endless_garbage_is_a_reset() {
    let mut rig = Rig::new(1024, 1024);
    let up = bring_up(&mut rig);
    // Command 10 is not used; the C would wedge on it. One bad command is skipped as an 8-byte message.
    let ev = rig.modem_says(vec![hdr(10, 1, 2), m_signals(up.m_cid, 0x8000_0000)]);
    assert_eq!(
        ev,
        vec![Event::Signals {
            channel: up.ch,
            signals: 0x8000_0000,
            enable: true
        }]
    );
    assert_eq!(rig.ap.violations(), 1);
    // Eight in a row is garbage.
    let ev = rig.modem_says((0..8).map(|i| hdr(100 + i, 0, 0)).collect());
    assert!(ev.contains(&Event::PeerReset {
        reason: ResetReason::Corrupt
    }));
    assert!(ev.contains(&Event::ChannelClosed { channel: up.ch }));
}

#[test]
fn oversized_claims_are_corruption_not_a_stall() {
    // An OPEN claiming 65535 name bytes, a TX_DATA claiming 4 GB, an INTENT claiming 4 G intents: none can fit in
    // the ring. The C would wait for them for ever (or, for INTENT, spin).
    let mut data = hdr(9, 5, 1);
    data.extend_from_slice(&0xffff_fff0u32.to_le_bytes());
    data.extend_from_slice(&0u32.to_le_bytes());
    for bad in [hdr(2, 1, 0xffff), data, hdr(5, 5, 0xffff_ffff)] {
        let mut rig = Rig::new(1024, 1024);
        let up = bring_up(&mut rig);
        let ev = rig.modem_says(vec![bad]);
        assert!(
            ev.contains(&Event::PeerReset {
                reason: ResetReason::Corrupt
            }),
            "{ev:?}"
        );
        assert!(ev.contains(&Event::ChannelClosed { channel: up.ch }));
    }
}

#[test]
fn incomplete_intent_waits_instead_of_spinning() {
    let mut rig = Rig::new(1024, 1024);
    let up = bring_up(&mut rig);
    // INTENT with count 3 but only the header and one pair written so far.
    let full = {
        let mut v = hdr(5, up.m_cid, 3);
        for i in 0..3u32 {
            v.extend_from_slice(&(100 + i).to_le_bytes());
            v.extend_from_slice(&(40 + i).to_le_bytes());
        }
        v
    };
    rig.modem.send(full[..16].to_vec());
    // Not a whole frame by the AP's reckoning, so nothing happens, and poll returns.
    rig.settle();
    assert_eq!(rig.ap.violations(), 0);
    rig.modem.send(full[16..].to_vec());
    rig.settle();
    // The three intents are usable now.
    rig.ap.send(up.ch, &[0; 101]).unwrap();
    rig.settle();
    assert!(matches!(&rig.modem.take()[..], [Msg::Data { iid: 41, .. }]));
}

// ---- peer reset -------------------------------------------------------------------------------------------------

fn restart_modem(rig: &mut Rig) {
    // A restarted modem re-initialises the whole shared area and starts its own state from scratch.
    let mut r = rig.ram.borrow_mut();
    for w in r.words.iter_mut() {
        *w = 0;
    }
    drop(r);
    rig.modem = Modem::new(&rig.ram);
}

#[test]
fn remote_crash_with_junk_indexes_resets_and_recovers() {
    let mut rig = Rig::new(1024, 1024);
    let up = bring_up(&mut rig);
    // The crash: rx_head becomes junk (`glink_smem_rx_avail` would return nonsense and peek outside the ring).
    rig.ram.borrow_mut().set_desc(3, 0xffff_fff0);
    let ev = rig.ap.poll();
    assert_eq!(
        ev,
        vec![
            Event::ChannelClosed { channel: up.ch },
            Event::PeerReset {
                reason: ResetReason::Fault(Fault::Index)
            }
        ]
    );
    assert_eq!(rig.ap.link_state(), LinkState::Down);
    assert_eq!(rig.ap.channel_state(up.ch), None);
    assert_eq!(rig.ap.send(up.ch, b"x"), Err(crate::Error::UnknownChannel));
    // While the indexes stay junk: quiet, no repeated events, no panic.
    for _ in 0..5 {
        assert!(rig.ap.poll().is_empty());
    }
    // The modem comes back, zeroes the area and proposes a version.
    restart_modem(&mut rig);
    rig.modem.send(m_version(1, 1));
    let ev = rig.settle();
    assert!(ev.is_empty());
    assert_eq!(rig.ap.link_state(), LinkState::Negotiating);
    let seen = rig.modem.take();
    assert!(
        seen.contains(&Msg::Version(1, 1)) && seen.contains(&Msg::VersionAck(1, 1)),
        "{seen:?}"
    );
    let ev = rig.modem_says(vec![m_version_ack(1, 1)]);
    assert_eq!(ev, vec![Event::LinkUp { features: 1 }]);
    // And the channel can be opened again, with new handles.
    rig.modem_says(vec![m_open(5, "IPCRTR")]);
    let seen = rig.modem.take();
    let Msg::Open(lcid, _) = seen[1].clone() else {
        panic!("{seen:?}")
    };
    let ev = rig.modem_says(vec![m_open_ack(lcid)]);
    let [Event::ChannelOpened { channel, .. }] = ev.as_slice() else {
        panic!("{ev:?}")
    };
    assert_ne!(*channel, up.ch);
}

#[test]
fn descriptor_zeroed_under_us_is_noticed() {
    let mut rig = Rig::new(1024, 1024);
    let up = bring_up(&mut rig);
    rig.ap.send(up.ch, b"x").unwrap();
    rig.settle();
    assert_ne!(rig.ram.borrow().desc(1), 0, "our head has moved");
    for i in 0..8 {
        rig.ram.borrow_mut().set_desc(i, 0);
    }
    let ev = rig.ap.poll();
    assert!(ev.contains(&Event::PeerReset {
        reason: ResetReason::Fault(Fault::OwnIndexChanged)
    }));
    assert!(ev.contains(&Event::ChannelClosed { channel: up.ch }));
    // The indexes are sane, so negotiation started at once with a fresh VERSION at the new head.
    assert_eq!(rig.ap.link_state(), LinkState::Negotiating);
    rig.modem = Modem::new(&rig.ram);
    rig.settle();
    assert_eq!(rig.modem.take(), vec![Msg::Version(1, 1)]);
}

#[test]
fn remote_tail_past_our_head_is_a_fault() {
    let mut rig = Rig::new(1024, 1024);
    let up = bring_up(&mut rig);
    rig.settle();
    let head = rig.ram.borrow().desc(1);
    rig.ram.borrow_mut().set_desc(0, (head + 64) % 1024);
    rig.ap.send(up.ch, b"x").unwrap();
    let ev = rig.ap.poll();
    assert!(
        ev.contains(&Event::PeerReset {
            reason: ResetReason::Fault(Fault::TailBeyondHead)
        }),
        "{ev:?}"
    );
}

#[test]
fn caller_requested_reset_after_remoteproc_crash() {
    let mut rig = Rig::new(1024, 1024);
    let up = bring_up(&mut rig);
    rig.ap
        .send(up.ch, b"stuck behind a missing intent")
        .unwrap();
    rig.settle();
    rig.modem.take();
    let ev = rig.ap.reset();
    assert_eq!(
        ev,
        vec![
            Event::ChannelClosed { channel: up.ch },
            Event::PeerReset {
                reason: ResetReason::Requested
            }
        ]
    );
    assert_eq!(rig.ap.tx_backlog(), 0);
    assert_eq!(rig.ap.features(), 1);
    rig.settle();
    // Unread data from the dead peer was dropped (rx_tail follows rx_head) and VERSION goes out again.
    assert_eq!(rig.modem.take(), vec![Msg::Version(1, 1)]);
}

#[test]
fn rx_ring_found_late_like_smem_item_480() {
    let ram = Ram::new(1024, 1024);
    ram.borrow_mut().rx_visible = false;
    let mut ap = Glink::new(Port::new(&ram, Side::Ap), ap_layout(&ram, false)).unwrap();
    let mut modem = Modem::new(&ram);
    modem.send(m_version(1, 1));
    // The modem has written, but item 480 does not exist for us yet: nothing is read, nothing breaks.
    assert!(ap.poll().is_empty());
    modem.pump_rx();
    assert_eq!(modem.take(), vec![Msg::Version(1, 1)]);
    ram.borrow_mut().rx_visible = true;
    assert!(ap.poll().is_empty());
    modem.pump_rx();
    assert_eq!(modem.take(), vec![Msg::VersionAck(1, 1)]);
}

#[test]
fn doorbell_is_rung_once_per_flush() {
    let mut rig = Rig::new(1024, 1024);
    let up = bring_up(&mut rig);
    rig.modem_says(vec![
        m_intent(up.m_cid, 64, 1),
        m_intent(up.m_cid, 64, 2),
        m_intent(up.m_cid, 64, 3),
    ]);
    let before = rig.ram.borrow().bells_to_remote;
    rig.ap.send(up.ch, b"one").unwrap();
    assert_eq!(rig.ram.borrow().bells_to_remote, before + 1);
    rig.ap.poll();
    assert_eq!(
        rig.ram.borrow().bells_to_remote,
        before + 1,
        "nothing written, no bell"
    );
}

#[test]
fn duplicate_remote_intents_and_opens_are_refused() {
    let mut rig = Rig::new(4096, 4096);
    let up = bring_up(&mut rig);
    // `idr_alloc(.., iid, iid + 1)` refuses an id that is already there: the second advertisement is ignored.
    rig.modem_says(vec![m_intent(up.m_cid, 10, 1), m_intent(up.m_cid, 500, 1)]);
    assert_eq!(rig.ap.violations(), 1);
    rig.ap.send(up.ch, &pattern(100, 1)).unwrap();
    rig.settle();
    assert_eq!(
        rig.modem.take(),
        vec![Msg::IntentReq {
            cid: up.ap_lcid,
            size: 100
        }]
    );
    // A second OPEN with a remote id that is in use is refused too, and so is a name that cannot be a C string.
    rig.modem_says(vec![m_open(up.m_cid, "OTHER")]);
    assert_eq!(rig.ap.violations(), 2);
    rig.modem_says(vec![m_open(
        40,
        "a-name-that-is-longer-than-thirty-one-bytes",
    )]);
    assert_eq!(rig.ap.violations(), 3);
    assert!(rig.modem.take().is_empty());
}
