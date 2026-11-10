//! Descriptor and probe tests on a configuration built here the way UVC 1.0 webcams lay theirs out. The camera's own recorded
//! descriptor is tested in `tests/recorded.rs`.

use alloc::vec::Vec;

use crate::desc::{self, Intervals};
use crate::probe::{self, Control, Probe, ProbeStep};

fn frame(subtype: u8, index: u8, w: u16, h: u16, size: u32, intervals: &[u32]) -> Vec<u8> {
    let mut d = alloc::vec![0, 0x24, subtype, index, 0];
    d.extend_from_slice(&w.to_le_bytes());
    d.extend_from_slice(&h.to_le_bytes());
    d.extend_from_slice(&(1_000_000u32).to_le_bytes());
    d.extend_from_slice(&(100_000_000u32).to_le_bytes());
    d.extend_from_slice(&size.to_le_bytes());
    d.extend_from_slice(&intervals[0].to_le_bytes());
    d.push(intervals.len() as u8);
    for i in intervals {
        d.extend_from_slice(&i.to_le_bytes());
    }
    d[0] = d.len() as u8;
    d
}

fn interface(number: u8, alt: u8, endpoints: u8, subclass: u8) -> Vec<u8> {
    alloc::vec![9, 4, number, alt, endpoints, 0x0e, subclass, 0, 0]
}

fn endpoint(address: u8, max_packet: u16) -> Vec<u8> {
    let m = max_packet.to_le_bytes();
    alloc::vec![7, 5, address, 0x05, m[0], m[1], 1]
}

pub(crate) fn sample() -> Vec<u8> {
    let mut d: Vec<u8> = alloc::vec![9, 2, 0, 0, 2, 1, 0, 0x80, 250];
    d.extend_from_slice(&[8, 0x0b, 0, 2, 0x0e, 3, 0, 0]);
    d.extend(interface(0, 0, 1, 1));
    // VC header: bcdUVC 1.00, wTotalLength, dwClockFrequency 15 MHz, one streaming interface.
    let mut vc = alloc::vec![13, 0x24, 1, 0x00, 0x01, 0, 0];
    vc.extend_from_slice(&15_000_000u32.to_le_bytes());
    vc.extend_from_slice(&[1, 1]);
    d.extend(vc);
    d.extend(interface(1, 0, 0, 2));
    d.extend_from_slice(&[14, 0x24, 1, 2, 0, 0, 0x81, 0, 3, 2, 1, 0, 1, 0]);
    // MJPEG, default frame 1.
    d.extend_from_slice(&[11, 0x24, 6, 1, 2, 1, 1, 0, 0, 0, 0]);
    d.extend(frame(7, 1, 1280, 720, 1_843_200, &[333_333, 666_666]));
    d.extend(frame(7, 2, 640, 480, 614_400, &[333_333]));
    d.extend_from_slice(&[6, 0x24, 0x0d, 1, 1, 4]);
    // YUY2.
    let mut yuy2 = alloc::vec![27, 0x24, 4, 2, 2, b'Y', b'U', b'Y', b'2'];
    yuy2.extend_from_slice(&[0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71]);
    yuy2.extend_from_slice(&[16, 1, 0, 0, 0, 0]);
    d.extend(yuy2);
    d.extend(frame(5, 1, 640, 480, 614_400, &[333_333, 666_666, 1_000_000]));
    d.extend(frame(5, 2, 1280, 720, 1_843_200, &[1_000_000]));
    d.extend(interface(1, 1, 1, 2));
    d.extend(endpoint(0x81, 0x0080));
    d.extend(interface(1, 2, 1, 2));
    d.extend(endpoint(0x81, 0x0400));
    d.extend(interface(1, 3, 1, 2));
    d.extend(endpoint(0x81, 0x1400));
    let total = (d.len() as u16).to_le_bytes();
    d[2] = total[0];
    d[3] = total[1];
    d
}

#[test]
fn parses_a_webcam() {
    let raw = sample();
    assert!(desc::is_video(&raw));
    let cam = desc::parse(&raw).unwrap();
    assert_eq!(cam.control_interface, 0);
    assert_eq!(cam.uvc_version, 0x0100);
    assert_eq!(cam.clock_frequency, 15_000_000);
    assert_eq!(cam.streaming.len(), 1);
    let s = &cam.streaming[0];
    assert_eq!((s.interface, s.endpoint, s.still_method), (1, 0x81, 2));
    assert_eq!(s.formats.len(), 2);
    let mjpg = s.format(1).unwrap();
    assert_eq!((mjpg.name(), mjpg.compressed, mjpg.default_frame), ("MJPG", true, 1));
    assert_eq!(mjpg.colour, desc::Colour { primaries: 1, transfer: 1, matrix: 4 });
    assert_eq!(mjpg.frames[0].intervals, Intervals::Discrete(alloc::vec![333_333, 666_666]));
    let yuy2 = s.format(2).unwrap();
    assert_eq!((yuy2.name(), yuy2.compressed, yuy2.bits_per_pixel), ("YUY2", false, 16));
    assert_eq!(yuy2.frame(2).unwrap().width, 1280);
    assert_eq!(s.alts.iter().map(|a| a.bytes_per_interval).collect::<Vec<_>>(), [128, 1024, 3072]);
    assert_eq!(s.max_bytes_per_interval(), 3072);
}

#[test]
fn refuses_truncated_and_short_descriptors() {
    let mut raw = sample();
    raw.truncate(raw.len() - 3);
    assert_eq!(desc::parse(&raw), Err(desc::Error::Truncated));
    let mut raw = sample();
    // The first MJPEG frame descriptor claims more intervals than it has room for.
    let at = raw.windows(3).position(|w| w == [34, 0x24, 7]).unwrap();
    raw[at + 25] = 9;
    assert_eq!(desc::parse(&raw), Err(desc::Error::Malformed));
    assert_eq!(desc::parse(&[9, 2, 9, 0, 0, 1, 0, 0x80, 50]), Err(desc::Error::NotVideo));
}

#[test]
fn chooses_the_smallest_big_enough_alt() {
    let cam = desc::parse(&sample()).unwrap();
    let s = &cam.streaming[0];
    assert_eq!(probe::choose_alt(s, 100).unwrap().alt, 1);
    assert_eq!(probe::choose_alt(s, 1000).unwrap().alt, 2);
    assert_eq!(probe::choose_alt(s, 3072).unwrap().alt, 3);
    assert_eq!(probe::choose_alt(s, 3073), None);
}

#[test]
fn frame_intervals_as_linux() {
    let cam = desc::parse(&sample()).unwrap();
    let f = cam.streaming[0].format(2).unwrap().frame(1).unwrap();
    assert_eq!(probe::try_frame_interval(f, 300_000), 333_333);
    assert_eq!(probe::try_frame_interval(f, 600_000), 666_666);
    assert_eq!(probe::try_frame_interval(f, 5_000_000), 1_000_000);
    let c = desc::Frame { intervals: Intervals::Continuous { min: 333_333, max: 2_000_000, step: 333_333 }, ..f.clone() };
    assert_eq!(probe::try_frame_interval(&c, 700_000), 666_666);
    assert_eq!(probe::try_frame_interval(&c, 10), 333_333);
    assert_eq!(probe::try_frame_interval(&c, 9_000_000), 2_000_000);
}

#[test]
fn control_wire_layout() {
    let c = Control {
        hint: 1,
        format_index: 2,
        frame_index: 1,
        frame_interval: 333_333,
        max_video_frame_size: 614_400,
        max_payload_transfer_size: 3072,
        ..Control::default()
    };
    let d = c.encode(26);
    assert_eq!(&d[0..8], &[1, 0, 2, 1, 0x15, 0x16, 0x05, 0x00]);
    assert_eq!(&d[18..26], &[0x00, 0x60, 0x09, 0x00, 0x00, 0x0c, 0x00, 0x00]);
    assert_eq!(c.encode(34).len(), 34);
}

#[test]
fn probe_sequence_and_fix_ups() {
    let cam = desc::parse(&sample()).unwrap();
    let s = &cam.streaming[0];
    let f = s.format(2).unwrap();
    let req = Control::request(f, f.frame(1).unwrap(), 333_333);
    let mut p = Probe::new(&cam, s, req);
    assert_eq!(p.size(), 26);
    let t = p.next().unwrap();
    assert_eq!((t.request, t.selector), (probe::SET_CUR, probe::VS_PROBE_CONTROL));
    p.answer(s, Some(&[])).unwrap();
    assert_eq!(p.next().unwrap().request, probe::GET_MIN);
    // Some cameras answer GET_MIN/GET_MAX with only wCompQuality.
    p.answer(s, Some(&[1, 0])).unwrap();
    let mut max = req;
    max.comp_quality = 10_000;
    p.answer(s, Some(&max.encode(26))).unwrap();
    assert_eq!(p.step(), ProbeStep::Set);
    p.answer(s, Some(&[])).unwrap();
    // The camera's answer: no frame size (an uncompressed one takes the descriptor's), a sign-extended bandwidth.
    let mut cur = req;
    cur.max_payload_transfer_size = 0xffff_0c00;
    p.answer(s, Some(&cur.encode(26))).unwrap();
    assert_eq!(p.step(), ProbeStep::Commit);
    assert_eq!(p.control().max_video_frame_size, 614_400);
    assert_eq!(p.control().max_payload_transfer_size, 3072);
    assert_eq!(p.control().clock_frequency, 15_000_000);
    assert_eq!(p.next().unwrap().selector, probe::VS_COMMIT_CONTROL);
    p.answer(s, Some(&[])).unwrap();
    assert_eq!(p.next(), None);
}

#[test]
fn probe_retries_once_when_the_bandwidth_is_too_high() {
    let cam = desc::parse(&sample()).unwrap();
    let s = &cam.streaming[0];
    let f = s.format(1).unwrap();
    let req = Control::request(f, f.frame(1).unwrap(), 333_333);
    let mut p = Probe::new(&cam, s, req);
    p.answer(s, Some(&[])).unwrap();
    let mut min = req;
    min.key_frame_rate = 5;
    p.answer(s, Some(&min.encode(26))).unwrap();
    p.answer(s, Some(&req.encode(26))).unwrap();
    p.answer(s, Some(&[])).unwrap();
    let mut cur = req;
    cur.max_payload_transfer_size = 4000;
    cur.max_video_frame_size = 400_000;
    p.answer(s, Some(&cur.encode(26))).unwrap();
    assert_eq!(p.step(), ProbeStep::Set);
    assert_eq!(p.control().key_frame_rate, 5);
    p.answer(s, Some(&[])).unwrap();
    p.answer(s, Some(&cur.encode(26))).unwrap();
    // Two rounds, as Linux: then the stream goes on with what the camera said.
    assert_eq!(p.step(), ProbeStep::Commit);
    assert!(p.answer(s, None).is_err());
}

#[test]
fn rules_packets_and_spans() {
    use crate::rules;
    assert_eq!(rules::packets(614_400, 3072), 32);
    assert_eq!(rules::packets(3000, 3072), 1);
    assert_eq!(rules::packets(0, 3072), 1);
    assert_eq!(rules::packets(6145, 3072), 3);
    let s = rules::packet_span(2, 3072, 100, 3 * 3072);
    assert!(s.ok && s.start == 6144 && s.len == 100);
    assert!(!rules::packet_span(3, 3072, 1, 3 * 3072).ok);
    assert!(!rules::packet_span(0, 3072, 3073, 3 * 3072).ok);
    assert!(!rules::packet_span(usize::MAX, 2, 0, 10).ok);
}

#[test]
fn rules_slots_never_share_a_buffer() {
    use crate::rules::{self, NONE};
    let ok = |s: rules::Slots| {
        s.fill < 3
            && (s.ready < 3 || s.ready == NONE)
            && (s.held < 3 || s.held == NONE)
            && s.fill != s.ready
            && s.fill != s.held
            && (s.ready == NONE || s.ready != s.held)
    };
    // Every order of the four operations, six deep, from the start.
    let ops: [fn(rules::Slots) -> rules::Slots; 4] = [rules::publish, |s| rules::acquire(s).slots, rules::release, rules::restart];
    let mut states = alloc::vec![rules::slots_new()];
    for _ in 0..6 {
        let mut next = Vec::new();
        for s in &states {
            for op in ops {
                let t = op(*s);
                assert!(ok(t), "{s:?} -> {t:?}");
                next.push(t);
            }
        }
        states = next;
    }
    let s = rules::publish(rules::slots_new());
    let a = rules::acquire(s);
    assert!(a.got && a.slots.held == 0);
    let s = rules::publish(rules::publish(a.slots));
    assert_eq!((s.held, s.ready), (0, 2));
}
