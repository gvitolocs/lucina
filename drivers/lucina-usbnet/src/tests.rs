//! The pump against a scripted mock driver.

extern crate std;
use std::collections::VecDeque;
use std::string::ToString;
use std::vec::Vec;

use super::*;

#[derive(Default)]
struct Mock {
    /// Results of the coming `poll_link` calls; when empty the last link state stays.
    links: VecDeque<Result<bool, BackendError>>,
    up: bool,
    link_polls: Vec<u32>,
    sent: Vec<Vec<u8>>,
    send_results: VecDeque<Result<(), BackendError>>,
    rx: VecDeque<Result<Vec<u8>, BackendError>>,
    receive_calls: usize,
}

impl EthernetBackend for Mock {
    fn mac_addr(&self) -> [u8; 6] {
        [0x02, 0x00, 0x5e, 0x00, 0x00, 0x02]
    }
    fn send(&mut self, frame: &[u8]) -> Result<(), BackendError> {
        let result = self.send_results.pop_front().unwrap_or(Ok(()));
        if result.is_ok() {
            self.sent.push(frame.to_vec());
        }
        result
    }
    fn receive(&mut self, out: &mut [u8]) -> Result<Option<usize>, BackendError> {
        self.receive_calls += 1;
        match self.rx.pop_front() {
            None => Ok(None),
            Some(Ok(frame)) => {
                out[..frame.len()].copy_from_slice(&frame);
                Ok(Some(frame.len()))
            }
            Some(Err(e)) => Err(e),
        }
    }
    fn max_frame_len(&self) -> usize {
        64
    }
    fn poll_link(&mut self, elapsed_ms: u32) -> Result<bool, BackendError> {
        self.link_polls.push(elapsed_ms);
        if let Some(next) = self.links.pop_front() {
            if let Ok(up) = next {
                self.up = up;
            }
            return next;
        }
        Ok(self.up)
    }
    fn link_text(&self) -> String {
        "1000/full".to_string()
    }
    fn stats_text(&self) -> String {
        "tx 1 rx 2".to_string()
    }
}

struct Queues {
    tx: VecDeque<Vec<u8>>,
    rx: Vec<Vec<u8>>,
    rx_room: usize,
}

impl Queues {
    fn new() -> Self {
        Self {
            tx: VecDeque::new(),
            rx: Vec::new(),
            rx_room: usize::MAX,
        }
    }
}

fn pass(pump: &mut Pump, mock: &mut Mock, q: &mut Queues, now: u64) -> Pass {
    let Queues { tx, rx, rx_room } = q;
    pump.pass(mock, now, &mut || tx.pop_front(), &mut |frame| {
        if rx.len() >= *rx_room {
            return false;
        }
        rx.push(frame.to_vec());
        true
    })
}

fn up_mock() -> Mock {
    Mock {
        up: true,
        ..Mock::default()
    }
}

#[test]
fn link_is_polled_at_once_then_every_second_with_the_elapsed_time() {
    let mut mock = Mock::default();
    let mut pump = Pump::new(&mock);
    let mut q = Queues::new();
    for now in [5000, 5004, 5999, 6000, 6500, 7100] {
        pass(&mut pump, &mut mock, &mut q, now);
    }
    assert_eq!(mock.link_polls, [0, 1000, 1100]);
}

#[test]
fn link_changes_are_events_with_link_and_stats_text() {
    let mut mock = Mock {
        links: VecDeque::from([Ok(false), Ok(true), Ok(true), Ok(false)]),
        ..Mock::default()
    };
    let mut pump = Pump::new(&mock);
    let mut q = Queues::new();
    let events: Vec<Vec<Event>> = (0..4)
        .map(|i| pass(&mut pump, &mut mock, &mut q, i * 1000).events)
        .collect();
    assert_eq!(
        events,
        [
            vec![],
            vec![Event::LinkUp("1000/full".into())],
            vec![],
            vec![Event::LinkDown("tx 1 rx 2".into())],
        ]
    );
    assert!(!pump.link_up());
}

#[test]
fn frames_go_both_ways_in_order() {
    let mut mock = up_mock();
    mock.rx = VecDeque::from([Ok(vec![1u8; 60]), Ok(vec![2u8; 14])]);
    let mut pump = Pump::new(&mock);
    let mut q = Queues::new();
    q.tx = VecDeque::from([vec![9u8; 60], vec![8u8; 42]]);
    let p = pass(&mut pump, &mut mock, &mut q, 0);
    assert_eq!((p.sent, p.received), (2, 2));
    assert_eq!(mock.sent, [vec![9u8; 60], vec![8u8; 42]]);
    assert_eq!(q.rx, [vec![1u8; 60], vec![2u8; 14]]);
    assert_eq!(pump.counters().tx_frames, 2);
    assert_eq!(pump.counters().rx_frames, 2);
}

#[test]
fn a_pass_moves_at_most_the_per_pass_limits() {
    let mut mock = up_mock();
    mock.rx = (0..MAX_RX_PER_PASS + 5)
        .map(|_| Ok(vec![0u8; 20]))
        .collect();
    let mut pump = Pump::new(&mock);
    let mut q = Queues::new();
    q.tx = (0..MAX_TX_PER_PASS + 3).map(|_| vec![0u8; 20]).collect();
    let p = pass(&mut pump, &mut mock, &mut q, 0);
    assert_eq!((p.sent, p.received), (MAX_TX_PER_PASS, MAX_RX_PER_PASS));
    let p = pass(&mut pump, &mut mock, &mut q, 4);
    assert_eq!((p.sent, p.received), (3, 5));
}

#[test]
fn frames_to_send_are_dropped_while_the_link_is_down() {
    let mut mock = Mock::default();
    let mut pump = Pump::new(&mock);
    let mut q = Queues::new();
    q.tx = VecDeque::from([vec![1u8; 20], vec![2u8; 20]]);
    let p = pass(&mut pump, &mut mock, &mut q, 0);
    assert_eq!(p.sent, 0);
    assert!(mock.sent.is_empty() && q.tx.is_empty());
    assert_eq!(pump.counters().tx_dropped, 2);
}

#[test]
fn a_full_rx_queue_loses_nothing_and_stops_taking_frames() {
    let mut mock = up_mock();
    mock.rx = (0..3).map(|i| Ok(vec![i as u8; 20])).collect();
    let mut pump = Pump::new(&mock);
    let mut q = Queues::new();
    q.rx_room = 1;
    let p = pass(&mut pump, &mut mock, &mut q, 0);
    assert_eq!(p.received, 1);
    assert_eq!(q.rx, [vec![0u8; 20]]);
    assert_eq!(pump.counters().rx_held, 1);
    assert_eq!(mock.rx.len(), 1, "the third frame stays with the chip");

    let p = pass(&mut pump, &mut mock, &mut q, 1);
    assert_eq!(p.received, 0, "still no room");
    assert_eq!(mock.rx.len(), 1);

    q.rx_room = 3;
    let p = pass(&mut pump, &mut mock, &mut q, 2);
    assert_eq!(p.received, 2);
    assert_eq!(
        q.rx,
        [vec![0u8; 20], vec![1u8; 20], vec![2u8; 20]],
        "in order"
    );
    assert_eq!(pump.counters().rx_frames, 3);
}

/// A chip with room for two frames per pass, as one transfer.
struct Batching {
    inner: Mock,
    room: usize,
    batches: Vec<usize>,
}

impl EthernetBackend for Batching {
    fn mac_addr(&self) -> [u8; 6] {
        self.inner.mac_addr()
    }
    fn send(&mut self, frame: &[u8]) -> Result<(), BackendError> {
        self.inner.send(frame)
    }
    fn send_frames(
        &mut self,
        next: &mut dyn FnMut() -> Option<Vec<u8>>,
        limit: usize,
    ) -> Result<usize, BackendError> {
        if self.room == 0 {
            return Ok(0);
        }
        self.room -= 1;
        let mut sent = 0;
        while sent < limit.min(2) && next().is_some() {
            sent += 1;
        }
        self.batches.push(sent);
        Ok(sent)
    }
    fn receive(&mut self, out: &mut [u8]) -> Result<Option<usize>, BackendError> {
        self.inner.receive(out)
    }
    fn poll_link(&mut self, elapsed_ms: u32) -> Result<bool, BackendError> {
        self.inner.poll_link(elapsed_ms)
    }
    fn link_text(&self) -> String {
        self.inner.link_text()
    }
    fn stats_text(&self) -> String {
        self.inner.stats_text()
    }
}

#[test]
fn a_chip_without_room_leaves_the_frames_queued_and_the_pass_ends() {
    let mut chip = Batching {
        inner: up_mock(),
        room: 2,
        batches: Vec::new(),
    };
    let mut pump = Pump::new(&chip);
    let mut tx: VecDeque<Vec<u8>> = (0..7).map(|_| vec![0u8; 20]).collect();
    let p = pump.pass(&mut chip, 0, &mut || tx.pop_front(), &mut |_| true);
    assert_eq!(p.sent, 4);
    assert_eq!(chip.batches, [2, 2]);
    assert_eq!(tx.len(), 3, "the rest waits for room");
    assert_eq!(pump.counters().tx_frames, 4);
    assert!(!p.gone());
}

#[test]
fn a_failed_send_loses_that_frame_only() {
    let mut mock = up_mock();
    mock.send_results = VecDeque::from([Err(BackendError::Failed)]);
    let mut pump = Pump::new(&mock);
    let mut q = Queues::new();
    q.tx = VecDeque::from([vec![1u8; 20], vec![2u8; 20]]);
    let p = pass(&mut pump, &mut mock, &mut q, 0);
    assert_eq!(p.sent, 1);
    assert_eq!(mock.sent, [vec![2u8; 20]]);
    assert_eq!(pump.counters().tx_errors, 1);
    assert!(!p.gone());
}

#[test]
fn a_failed_receive_ends_the_rx_part_of_the_pass() {
    let mut mock = up_mock();
    mock.rx = VecDeque::from([Err(BackendError::Failed), Ok(vec![1u8; 20])]);
    let mut pump = Pump::new(&mock);
    let mut q = Queues::new();
    let p = pass(&mut pump, &mut mock, &mut q, 0);
    assert_eq!(p.received, 0);
    assert_eq!(pump.counters().rx_errors, 1);
    let p = pass(&mut pump, &mut mock, &mut q, 4);
    assert_eq!(p.received, 1);
}

#[test]
fn unplug_in_any_call_is_gone_and_the_pump_stops() {
    for case in 0..3 {
        let mut mock = up_mock();
        let mut q = Queues::new();
        match case {
            0 => mock.links = VecDeque::from([Err(BackendError::Gone)]),
            1 => {
                mock.send_results = VecDeque::from([Err(BackendError::Gone)]);
                q.tx.push_back(vec![0u8; 20]);
            }
            _ => mock.rx = VecDeque::from([Err(BackendError::Gone)]),
        }
        let mut pump = Pump::new(&mock);
        let p = pass(&mut pump, &mut mock, &mut q, 0);
        assert!(p.gone(), "case {case}");
        assert!(!pump.link_up());
        let receives = mock.receive_calls;
        let p = pass(&mut pump, &mut mock, &mut q, 5000);
        assert_eq!(p, Pass::default());
        assert_eq!(mock.receive_calls, receives);
    }
}

#[test]
fn an_adapter_that_fails_every_call_is_given_up() {
    let mut mock = up_mock();
    let mut pump = Pump::new(&mock);
    let mut q = Queues::new();
    let mut gone_at = None;
    for i in 0..MAX_FAILED_PASSES as u64 + 5 {
        mock.rx.push_back(Err(BackendError::Failed));
        if pass(&mut pump, &mut mock, &mut q, 1 + i * 4).gone() {
            gone_at = Some(i);
            break;
        }
    }
    // The first pass's link poll succeeded, so the count starts with the second.
    assert_eq!(gone_at, Some(MAX_FAILED_PASSES as u64));
}

#[test]
fn one_good_call_resets_the_failure_count() {
    let mut mock = up_mock();
    let mut pump = Pump::new(&mock);
    let mut q = Queues::new();
    for i in 0..3 * MAX_FAILED_PASSES as u64 {
        if i % 50 == 0 {
            mock.rx.push_back(Ok(vec![0u8; 20]));
        }
        mock.rx.push_back(Err(BackendError::Failed));
        assert!(!pass(&mut pump, &mut mock, &mut q, 1 + i * 4).gone());
    }
}

#[test]
fn rx_buffer_is_the_backends_max_frame_len() {
    let mock = Mock::default();
    let pump = Pump::new(&mock);
    assert_eq!(pump.rx_buf.len(), 64);
}

#[test]
fn mac_text() {
    assert_eq!(
        format_mac([0x02, 0x00, 0x5e, 0x00, 0x00, 0x02]),
        "02:00:5e:00:00:02"
    );
}
