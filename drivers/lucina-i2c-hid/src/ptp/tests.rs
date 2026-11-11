//! The Galaxy Book Go touchpad's own report descriptor, as the laptop printed it (buildhost
//! `~/scratch/juno-first/main17/install-kmsg-20261104-220517.log`, `[hid] i2c@a90000/touchpad@40: report descriptor 318
//! bytes`), and reports laid out as that descriptor says.

extern crate std;

use super::*;
use std::vec;
use std::vec::Vec;

/// 14e5:650e, 318 bytes.
pub(crate) const BOOK_GO_TOUCHPAD: &str = "05010902a10185080901a100050919012902250175019502810295068103050109300931751095021681ff267f008106c0c0050d090ea10185040922a10209521500250a75089501b102c00922a100850909570958750195022501b1029506b103c0c0050d0905a10185010922a102150025010942094795027501810295017503250509518102750195038103050115002654077510550e65130930350046d50195018102268c0446230109318102050d150025649502c0550c66011047ffff000027ffff000075109501095681020954257f950175088102050909011500250175019501810295078103050d85020955095975049502250fb102050d850a09607501950115002501b1029507b1030600ff850509c5150026ff007508960001b102c00600ff0901a101150026ff007508850695110901810209019102c0";

pub(crate) fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

pub(crate) fn format() -> PtpFormat {
    let d = unhex(BOOK_GO_TOUCHPAD);
    assert_eq!(d.len(), 318);
    PtpFormat::parse(&d).expect("a touch pad")
}

/// Report 1 as the descriptor lays it out: tip bit 0, confidence bit 1, contact ID bits 2-4, X 8-23, Y 24-39, scan time
/// 40-55, contact count 56-63, button 1 bit 64.
#[allow(clippy::too_many_arguments)]
pub(crate) fn report(
    tip: bool,
    confidence: bool,
    id: u8,
    x: u16,
    y: u16,
    scan: u16,
    count: u8,
    button: bool,
) -> [u8; 10] {
    let b0 = u8::from(tip) | u8::from(confidence) << 1 | (id & 7) << 2;
    let [x0, x1] = x.to_le_bytes();
    let [y0, y1] = y.to_le_bytes();
    let [s0, s1] = scan.to_le_bytes();
    [1, b0, x0, x1, y0, y1, s0, s1, count, u8::from(button)]
}

#[test]
fn the_book_go_touchpad_is_a_precision_touchpad() {
    let f = format();
    assert_eq!(f.report_id, 1);
    assert_eq!(f.fingers().len(), 1, "hybrid mode: one contact per report");
    let finger = f.fingers()[0];
    let s = |bit_offset, size| {
        Some(Scalar {
            bit_offset,
            size,
            signed: false,
        })
    };
    assert_eq!(finger.tip, s(0, 1));
    assert_eq!(finger.confidence, s(1, 1));
    assert_eq!(finger.contact_id, s(2, 3));
    assert_eq!(finger.in_range, None);
    assert_eq!(
        f.x.field,
        Scalar {
            bit_offset: 8,
            size: 16,
            signed: false
        }
    );
    assert_eq!(
        f.y.field,
        Scalar {
            bit_offset: 24,
            size: 16,
            signed: false
        }
    );
    assert_eq!((f.x.min, f.x.max, f.y.min, f.y.max), (0, 1876, 0, 1164));
    // hidinput_calc_abs_res: 1876 units over 4.69 in = 1876000 / 119126 -> 16; 1164000 / 73914 -> 16.
    assert_eq!((f.x.resolution, f.y.resolution), (16, 16));
    assert_eq!(f.scan_time, s(40, 16));
    assert_eq!(f.scan_time_max, 65535);
    assert_eq!(f.contact_count, s(56, 8));
    assert_eq!(f.buttons, [Some(64), None, None]);
    assert_eq!(f.buttons_count, 1);
}

#[test]
fn the_feature_reports_are_found() {
    let f = format();
    let ff = |report_id, bit_offset, size, logical_max| {
        Some(FeatureField {
            report_id,
            bit_offset,
            size,
            logical_max,
        })
    };
    assert_eq!(f.input_mode, ff(4, 0, 8, 10));
    assert_eq!(f.surface_switch, ff(9, 0, 1, 1));
    assert_eq!(f.button_switch, ff(9, 1, 1, 1));
    assert_eq!(f.contact_max, ff(2, 0, 4, 15));
    assert_eq!(f.button_type, ff(2, 4, 4, 15));
    assert_eq!(f.latency, ff(10, 0, 1, 1));
    assert_eq!(f.blob_report, Some(5));
    assert_eq!(f.feature_len(4), Some(2));
    assert_eq!(f.feature_len(5), Some(257));
    assert_eq!(f.feature_len(2), Some(2));
}

#[test]
fn probe_reads_contact_max_and_pad_type_then_the_blob() {
    let reads: Vec<_> = format().probe_reads().collect();
    assert_eq!(
        reads,
        [
            ProbeRead {
                report_id: 2,
                len: 2
            },
            ProbeRead {
                report_id: 5,
                len: 257
            }
        ]
    );
}

#[test]
fn mode_reports_are_linux_mt_set_modes_in_descriptor_order() {
    let f = format();
    let normal: Vec<_> = f
        .mode_reports(Latency::Normal, ReportMode::ALL)
        .map(|m| (m.report_id, m.data().to_vec()))
        .collect();
    // Report 4 Input Mode = 3; report 9 Surface Switch (bit 0) and Button Switch (bit 1) on; report 10 Latency normal.
    // Report 2 (Contact Count Maximum) is not written: MT_CLS_WIN_8 has no maxcontacts.
    assert_eq!(normal, [(4, vec![3]), (9, vec![3]), (10, vec![0])]);
    // mt_suspend without wakeup: high latency, nothing reported.
    let suspend: Vec<_> = f
        .mode_reports(Latency::High, ReportMode::NONE)
        .map(|m| (m.report_id, m.data().to_vec()))
        .collect();
    assert_eq!(suspend, [(4, vec![3]), (9, vec![0]), (10, vec![1])]);
}

#[test]
fn max_contacts_follows_mt_feature_mapping() {
    let f = format();
    assert_eq!(f.max_contacts(Some(5)), 5);
    // A zero answer falls back to the field's logical maximum (15), kept to the slots there are.
    assert_eq!(f.max_contacts(Some(0)), MAX_SLOTS);
    assert_eq!(f.max_contacts(None), MAX_SLOTS);
    let blob = [2u8, 0x05];
    assert_eq!(f.read_feature(f.contact_max.unwrap(), &blob), Some(5));
    assert_eq!(
        f.read_feature(f.button_type.unwrap(), &blob),
        Some(BUTTON_TYPE_CLICKPAD)
    );
}

#[test]
fn the_mouse_mode_report_is_not_a_touch_report() {
    let f = format();
    assert!(!f.is_touch_report(&[8, 0, 1, 0, 0xff, 0xff]));
    assert!(f.is_touch_report(&report(true, true, 0, 0, 0, 0, 1, false)));
}

fn run(c: &mut Contacts, reports: &[[u8; 10]], t: u64) -> (Vec<Frame>, Vec<MtEvent>) {
    let mut frames = Vec::new();
    let mut events = Vec::new();
    for r in reports {
        c.report(r, t, &mut |f| frames.push(*f), &mut |e| events.push(e));
    }
    (frames, events)
}

fn active(f: &Frame) -> Vec<(i32, i32, i32, i32)> {
    f.slots[..f.num_slots]
        .iter()
        .filter(|s| s.active())
        .map(|s| (s.tracking_id, s.tool, s.x, s.y))
        .collect()
}

fn ev(kind: u16, code: u16, value: i32) -> MtEvent {
    MtEvent { kind, code, value }
}

#[test]
fn one_finger_gives_linux_protocol_b_events() {
    let mut c = Contacts::new(format(), 5);
    let (frames, events) = run(&mut c, &[report(true, true, 0, 100, 200, 10, 1, false)], 0);
    assert_eq!(frames.len(), 1);
    assert_eq!(active(&frames[0]), [(0, MT_TOOL_FINGER, 100, 200)]);
    assert_eq!(
        events,
        [
            // Slot 0 is the current one already: no ABS_MT_SLOT.
            ev(EV_ABS, ABS_MT_TRACKING_ID, 0),
            ev(EV_ABS, ABS_MT_POSITION_X, 100),
            ev(EV_ABS, ABS_MT_POSITION_Y, 200),
            ev(EV_KEY, BTN_TOUCH, 1),
            ev(EV_KEY, BTN_TOOL_FINGER, 1),
            ev(EV_ABS, ABS_X, 100),
            ev(EV_ABS, ABS_Y, 200),
            ev(EV_SYN, SYN_REPORT, 0),
        ]
    );
    // Lift: tracking ID -1, BTN_TOUCH and the tool key off.
    let (frames, events) = run(
        &mut c,
        &[report(false, true, 0, 100, 200, 20, 1, false)],
        1000,
    );
    assert!(active(&frames[0]).is_empty());
    assert_eq!(
        events,
        [
            ev(EV_ABS, ABS_MT_TRACKING_ID, -1),
            ev(EV_KEY, BTN_TOUCH, 0),
            ev(EV_KEY, BTN_TOOL_FINGER, 0),
            ev(EV_SYN, SYN_REPORT, 0),
        ]
    );
}

#[test]
fn hybrid_mode_frames_wait_for_the_contact_count() {
    let mut c = Contacts::new(format(), 5);
    // Two fingers down: the first report says 2, the second 0, same scan time.
    let (frames, _) = run(
        &mut c,
        &[
            report(true, true, 0, 100, 200, 10, 2, false),
            report(true, true, 1, 900, 210, 10, 0, false),
        ],
        0,
    );
    assert_eq!(frames.len(), 1, "one frame for two reports");
    assert_eq!(active(&frames[0]), [(0, 0, 100, 200), (1, 0, 900, 210)]);
    // They move: still one frame per pair, same tracking IDs.
    let (frames, events) = run(
        &mut c,
        &[
            report(true, true, 0, 110, 230, 90, 2, false),
            report(true, true, 1, 910, 240, 90, 0, false),
        ],
        8000,
    );
    assert_eq!(active(&frames[0]), [(0, 0, 110, 230), (1, 0, 910, 240)]);
    assert!(events.contains(&ev(EV_ABS, ABS_MT_SLOT, 1)));
    assert!(events.contains(&ev(EV_ABS, ABS_MT_SLOT, 0)));
    // Scan time 10 -> 90 is 80 x 100 us.
    assert_eq!(frames[0].timestamp_us, 8000);
    // Both lift.
    let (frames, _) = run(
        &mut c,
        &[
            report(false, true, 0, 110, 230, 170, 2, false),
            report(false, true, 1, 910, 240, 170, 0, false),
        ],
        16000,
    );
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].count(), 0);
}

#[test]
fn a_button_only_report_with_count_zero_starts_a_frame_if_the_scan_time_changed() {
    let mut c = Contacts::new(format(), 5);
    let (frames, _) = run(&mut c, &[report(true, true, 0, 100, 200, 10, 1, false)], 0);
    assert_eq!(frames.len(), 1);
    // A first packet with count 0 but a new scan time is a frame of its own (MT_QUIRK_WIN8_PTP_BUTTONS).
    let (frames, events) = run(
        &mut c,
        &[report(true, true, 0, 100, 200, 20, 0, true)],
        1000,
    );
    assert_eq!(frames.len(), 1);
    assert!(frames[0].left);
    assert!(events.contains(&ev(EV_KEY, BTN_LEFT, 1)));
}

#[test]
fn a_contact_without_confidence_is_a_palm_and_lifts_in_its_own_frame() {
    let mut c = Contacts::new(format(), 5);
    run(&mut c, &[report(true, true, 0, 100, 200, 10, 1, false)], 0);
    // Confidence lost while down: a palm.
    let (frames, events) = run(
        &mut c,
        &[report(true, false, 0, 120, 220, 20, 1, false)],
        1000,
    );
    assert_eq!(active(&frames[0]), [(0, MT_TOOL_PALM, 120, 220)]);
    assert!(events.contains(&ev(EV_ABS, ABS_MT_TOOL_TYPE, MT_TOOL_PALM)));
    // Lifted without confidence: still reported active as a palm in this frame, then released in a second frame.
    let (frames, _) = run(
        &mut c,
        &[report(false, false, 0, 120, 220, 30, 1, false)],
        2000,
    );
    assert_eq!(frames.len(), 2);
    assert_eq!(active(&frames[0]), [(0, MT_TOOL_PALM, 120, 220)]);
    assert_eq!(frames[1].count(), 0);
}

#[test]
fn a_duplicate_contact_in_one_frame_is_ignored() {
    let mut c = Contacts::new(format(), 5);
    // Count 2, but the same contact twice: the second is MT_QUIRK_IGNORE_DUPLICATES' -EAGAIN and not received.
    let (frames, _) = run(
        &mut c,
        &[
            report(true, true, 0, 100, 200, 10, 2, false),
            report(true, true, 0, 300, 300, 10, 0, false),
        ],
        0,
    );
    assert!(frames.is_empty(), "still waiting for the second contact");
    let (frames, _) = run(&mut c, &[report(true, true, 1, 900, 210, 10, 0, false)], 0);
    assert_eq!(active(&frames[0]), [(0, 0, 100, 200), (1, 0, 900, 210)]);
}

#[test]
fn sticky_fingers_are_released_after_100_ms() {
    let mut c = Contacts::new(format(), 5);
    run(
        &mut c,
        &[report(true, true, 0, 100, 200, 10, 1, false)],
        5_000,
    );
    assert_eq!(c.sticky_deadline(), Some(105_000));
    let mut frames = Vec::new();
    c.tick(104_999, &mut |f| frames.push(*f), &mut |_| {});
    assert!(frames.is_empty());
    c.tick(105_000, &mut |f| frames.push(*f), &mut |_| {});
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].count(), 0);
    assert_eq!(c.sticky_deadline(), None);
}

#[test]
fn the_timestamp_restarts_after_a_second_without_reports() {
    let mut c = Contacts::new(format(), 5);
    let (f1, _) = run(&mut c, &[report(true, true, 0, 1, 1, 100, 1, false)], 0);
    let (f2, _) = run(&mut c, &[report(true, true, 0, 2, 2, 180, 1, false)], 8_000);
    let (f3, _) = run(
        &mut c,
        &[report(false, true, 0, 2, 2, 200, 1, false)],
        1_100_000,
    );
    assert_eq!(
        (f1[0].timestamp_us, f2[0].timestamp_us, f3[0].timestamp_us),
        (0, 8000, 0)
    );
    // Wrap at the scan time's logical maximum.
    let (f4, _) = run(
        &mut c,
        &[report(true, true, 0, 1, 1, 65530, 1, false)],
        1_101_000,
    );
    let (f5, _) = run(
        &mut c,
        &[report(true, true, 0, 1, 1, 10, 1, false)],
        1_102_000,
    );
    // 10 - 65530 + 65535 (`delta += app->scantime_logical_max`) = 15 scan units.
    assert_eq!(f5[0].timestamp_us - f4[0].timestamp_us, 15 * 100);
}
