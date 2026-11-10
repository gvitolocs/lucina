//! Garbage in: random and truncated ring contents. The checks are that `poll()` returns (so no panic and no
//! unbounded loop) and, to make "returns" mean something, that the number of word accesses in one poll stays within
//! a bound worked out from the ring sizes: every command consumes at least 8 bytes of a ring whose contents do not
//! change during a poll, so a poll cannot do more than a small multiple of the ring sizes in work.

use std::cell::RefCell;
use std::rc::Rc;
use std::vec;
use std::vec::Vec;

use super::model::{
    Msg, Rig, bring_up, m_close, m_data, m_intent, m_open, m_open_ack, m_signals, m_version,
    m_version_ack,
};
use super::support::{Port, Ram, Rng, ap_layout};
use crate::{Event, Glink, Side};

fn work(ram: &Rc<RefCell<Ram>>) -> u64 {
    let r = ram.borrow();
    r.reads + r.writes
}

fn bound(tx: usize, rx: usize) -> u64 {
    16 * (tx + rx) as u64 + 10_000
}

fn poll_bounded(ap: &mut Glink<Port>, ram: &Rc<RefCell<Ram>>, tx: usize, rx: usize) -> Vec<Event> {
    let before = work(ram);
    let ev = ap.poll();
    let used = work(ram) - before;
    assert!(used <= bound(tx, rx), "poll did {used} word accesses");
    ev
}

#[test]
fn random_ring_contents_and_indexes() {
    let (tx, rx) = (256usize, 512usize);
    for seed in 1..=400u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
        let ram = Ram::new(tx, rx);
        let mut ap = Glink::new(
            Port::new(&ram, Side::Ap),
            ap_layout(&ram, rng.below(4) != 0),
        )
        .unwrap();
        // The remote's ring is noise; a quarter of the time with a plausible command at the start.
        {
            let mut r = ram.borrow_mut();
            let base = r.rx_region().off / 4;
            for w in 0..rx / 4 {
                r.words[base + w] = rng.next() as u32;
            }
            if rng.below(4) == 0 {
                r.words[base] = (rng.below(18) as u32) | ((rng.below(8) as u32) << 16);
                r.words[base + 1] = rng.below(40) as u32;
            }
        }
        for round in 0..12 {
            {
                let mut r = ram.borrow_mut();
                match rng.below(6) {
                    // A believable head, a junk head, a clobbered descriptor word, a wild remote tail, or nothing.
                    0 => r.set_desc(3, (rng.below(rx as u64 / 8) * 8) as u32),
                    1 => r.set_desc(3, rng.next() as u32),
                    2 => {
                        let i = rng.below(4) as usize;
                        r.set_desc(i, rng.next() as u32);
                    }
                    3 => r.set_desc(0, (rng.below(tx as u64 / 8) * 8) as u32),
                    _ => {}
                }
            }
            // Calls from above, interleaved.
            match rng.below(6) {
                0 => {
                    let _ = ap.open(["IPCRTR", "DATA", "X"][rng.below(3) as usize]);
                }
                1 => {
                    let _ = ap.send(
                        crate::ChannelId(rng.below(4) as u32 + 1),
                        &vec![7; rng.below(3000) as usize],
                    );
                }
                2 => {
                    let _ = ap.close(crate::ChannelId(rng.below(4) as u32 + 1));
                }
                3 => {
                    ap.tick(round * 4000);
                }
                _ => {}
            }
            let _ = poll_bounded(&mut ap, &ram, tx, rx);
        }
    }
}

/// A frame of a random kind with random, mostly small, parameters, optionally cut short.
fn random_frame(rng: &mut Rng, m_cid: u16) -> Vec<u8> {
    let cmd = rng.below(20) as u16;
    let p1 = match rng.below(4) {
        0 => m_cid,
        1 => rng.below(8) as u16,
        2 => 0xffff,
        _ => rng.next() as u16,
    };
    let p2 = match rng.below(4) {
        0 => rng.below(6) as u32,
        1 => rng.below(2000) as u32,
        2 => rng.next() as u32,
        _ => 0,
    };
    let mut f = Vec::new();
    f.extend_from_slice(&cmd.to_le_bytes());
    f.extend_from_slice(&p1.to_le_bytes());
    f.extend_from_slice(&p2.to_le_bytes());
    let extra = rng.below(48) as usize;
    for _ in 0..extra {
        f.push(rng.next() as u8);
    }
    if rng.below(5) == 0 && f.len() > 8 {
        f.truncate(8 + rng.below(f.len() as u64 - 8) as usize);
    }
    f
}

#[test]
fn random_frames_after_a_handshake() {
    let (tx, rx) = (512usize, 512usize);
    for seed in 1..=150u64 {
        let mut rng = Rng(seed.wrapping_mul(0xd1b5_4a32_d192_ed03) | 1);
        let mut rig = Rig::new(tx, rx);
        let up = bring_up(&mut rig);
        let mut chans = vec![up.ch];
        for _ in 0..80 {
            match rng.below(8) {
                0 => {
                    if let Ok(c) = rig
                        .ap
                        .open(["IPCRTR", "DATA", "QMI"][rng.below(3) as usize])
                    {
                        chans.push(c);
                    }
                }
                1 => {
                    let c = chans[rng.below(chans.len() as u64) as usize];
                    let _ = rig.ap.send(c, &vec![5; 1 + rng.below(700) as usize]);
                }
                2 => {
                    let c = chans[rng.below(chans.len() as u64) as usize];
                    let _ = rig.ap.close(c);
                }
                3 => {
                    rig.ap.tick(rng.below(100_000));
                }
                _ => {
                    let f = random_frame(&mut rng, up.m_cid);
                    rig.modem.send(f);
                }
            }
            let before = work(&rig.ram);
            let _ = rig.ap.poll();
            assert!(work(&rig.ram) - before <= bound(tx, rx));
            // Whatever the AP wrote must still parse for the model (it asserts on malformed frames).
            rig.modem.pump_rx();
            rig.modem.retry();
            // Back-pressure keeps the AP's own queue bounded however much the remote asks for.
            assert!(rig.ap.tx_backlog() < 10_000);
        }
        // After all that, a fresh restart of the remote brings the link back.
        let _ = rig.ap.reset();
        rig.settle();
        assert!(
            rig.modem.take().contains(&Msg::Version(1, 1))
                || rig.ap.link_state() != crate::LinkState::Down
        );
    }
}

/// A scripted modem-to-AP stream of a whole session, as padded frames.
fn session_stream() -> (Vec<u8>, Vec<usize>) {
    let payload: Vec<u8> = (0..300u32).map(|i| (i * 7) as u8).collect();
    let frames = vec![
        m_version_ack(1, 1),
        m_version(1, 1),
        m_open(5, "IPCRTR"),
        m_open_ack(1),
        m_intent(5, 512, 9),
        m_data(false, 5, 1, &payload[..100], 200),
        m_data(true, 5, 1, &payload[100..200], 100),
        m_data(true, 5, 1, &payload[200..], 0),
        m_signals(5, 0x8000_0000),
        m_close(5),
    ];
    let mut stream = Vec::new();
    let mut ends = Vec::new();
    for mut f in frames {
        f.resize((f.len() + 7) & !7, 0);
        stream.extend(f);
        ends.push(stream.len());
    }
    (stream, ends)
}

#[test]
fn truncated_streams_at_every_byte() {
    let (stream, ends) = session_stream();
    let (tx, rx) = (1024usize, 4096usize);
    let mut data_events = Vec::new();
    for k in 0..=stream.len() {
        let ram = Ram::new(tx, rx);
        let mut ap = Glink::new(Port::new(&ram, Side::Ap), ap_layout(&ram, true)).unwrap();
        ram.borrow_mut().poke_rx(0, &stream[..k]);
        ram.borrow_mut().set_desc(3, k as u32);
        let mut ev = poll_bounded(&mut ap, &ram, tx, rx);
        ev.extend(poll_bounded(&mut ap, &ram, tx, rx));
        let datas = ev
            .iter()
            .filter(|e| matches!(e, Event::Data { .. }))
            .count();
        // A writer publishes its head only after padding a frame to 8 bytes. A head inside a frame's padding is
        // garbage like any other: it only has to be survived (it was, above), not made sense of.
        if k % 8 != 0 {
            continue;
        }
        // Nothing is acted on before its whole frame is in.
        let whole = ends.iter().filter(|&&e| e <= k).count();
        if whole < 8 {
            assert_eq!(datas, 0, "cut at {k}");
        }
        if whole >= 8 {
            assert_eq!(datas, 1, "cut at {k}");
            let Some(Event::Data { data, .. }) =
                ev.iter().find(|e| matches!(e, Event::Data { .. }))
            else {
                panic!()
            };
            assert_eq!(data.len(), 300);
        }
        data_events.push(datas);
        // The rest of the stream arriving later gives the same result as all at once.
        ram.borrow_mut().poke_rx(0, &stream);
        ram.borrow_mut().set_desc(3, stream.len() as u32);
        let rest = poll_bounded(&mut ap, &ram, tx, rx);
        let total = datas
            + rest
                .iter()
                .filter(|e| matches!(e, Event::Data { .. }))
                .count();
        assert_eq!(total, 1, "cut at {k}");
        let closed = ev
            .iter()
            .chain(rest.iter())
            .any(|e| matches!(e, Event::ChannelClosed { .. }));
        assert!(closed, "cut at {k}: the final CLOSE was handled");
    }
    assert!(data_events.windows(2).all(|w| w[0] <= w[1]));
}

#[test]
fn rx_loop_is_bounded_when_the_remote_never_stops_filling() {
    // A ring full of valid 8-byte READ_NOTIF commands: every poll drains it in one go and returns.
    let (tx, rx) = (256usize, 512usize);
    let ram = Ram::new(tx, rx);
    let mut ap = Glink::new(Port::new(&ram, Side::Ap), ap_layout(&ram, true)).unwrap();
    for i in 0..(rx / 8 - 1) {
        ram.borrow_mut().poke_rx(i * 8, &[13, 0, 0, 0, 0, 0, 0, 0]);
    }
    ram.borrow_mut().set_desc(3, (rx - 8) as u32);
    let before = ram.borrow().bells_to_remote;
    let ev = poll_bounded(&mut ap, &ram, tx, rx);
    assert!(ev.is_empty());
    assert_eq!(ram.borrow().bells_to_remote - before, (rx / 8 - 1) as u32);
    assert_eq!(ram.borrow().desc(2), (rx - 8) as u32);
    assert!(!ap.rx_pending());
}

#[test]
fn control_backlog_applies_back_pressure() {
    // A remote that sends INTENT_REQ after INTENT_REQ while never reading our ring: each is answered with two
    // frames, our ring fills, and the AP stops reading instead of queueing without limit.
    let mut rig = Rig::new(128, 4096);
    let up = bring_up(&mut rig);
    for _ in 0..400 {
        rig.modem.send(super::model::m_intent_req(up.m_cid, 16));
    }
    for _ in 0..20 {
        let _ = rig.ap.poll();
        rig.modem.retry();
    }
    assert!(
        rig.ap.tx_backlog() <= 140,
        "backlog {}",
        rig.ap.tx_backlog()
    );
    assert!(rig.ap.rx_pending());
    // Once the modem reads, the rest gets through.
    for _ in 0..4000 {
        rig.modem.pump_rx();
        rig.modem.retry();
        let _ = rig.ap.poll();
        if rig.modem.idle() && !rig.ap.rx_pending() && rig.ap.tx_backlog() == 0 {
            break;
        }
    }
    assert!(!rig.ap.rx_pending());
}
