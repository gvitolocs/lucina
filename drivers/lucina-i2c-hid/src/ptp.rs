//! Windows Precision Touchpads (Microsoft's "Precision Touchpad Required HID Top-Level Collections"), the way Linux's
//! `drivers/hid/hid-multitouch.c` (7.1.10) drives them as class `MT_CLS_WIN_8`, and `drivers/input/input-mt.c` turns
//! their contacts into slots. The Galaxy Book Go's touchpad (14e5:650e) is one, in hybrid mode: one contact per input
//! report, the contact count in the first report of a frame.
//!
//! * [`PtpFormat::parse`] finds the Touch Pad collection's input report (per finger: tip, confidence, contact ID, X, Y;
//!   per report: scan time, contact count, buttons) and the feature reports that set its mode (Input Mode, Surface and
//!   Button Switch, Latency Mode) or describe it (Contact Count Maximum and Pad Type, the certification blob).
//! * [`PtpFormat::mode_reports`] are the SET_REPORTs of `mt_set_modes`, in Linux's order; [`PtpFormat::probe_reads`] the
//!   GET_REPORTs `mt_feature_mapping` makes at probe.
//! * [`Contacts`] is `mt_touch_report`, `mt_process_slot` and `mt_sync_frame` with the `MT_CLS_WIN_8` quirks, and
//!   `input_mt_get_slot_by_key`, `input_mt_report_slot_state`, `input_mt_sync_frame`: reports in, whole frames of slots
//!   out ([`Frame`]), plus the evdev events Linux would send for them ([`MtEvent`]).
//!
//! What the frames become (pointer motion, scrolling, taps) is libinput's part: [`crate::touchpad`].

use crate::tp_rules::{self, expected_contacts, frame_ends, takes_contact};
use crate::{InputItem, MAIN_FEATURE, MAIN_INPUT, Scalar, bits, usage, walk_items};

const DG_TOUCH_PAD: u32 = usage(0x0d, 0x05);
const DG_IN_RANGE: u32 = usage(0x0d, 0x32);
const DG_TIP_SWITCH: u32 = usage(0x0d, 0x42);
const DG_CONFIDENCE: u32 = usage(0x0d, 0x47);
const DG_CONTACT_ID: u32 = usage(0x0d, 0x51);
const DG_INPUT_MODE: u32 = usage(0x0d, 0x52);
const DG_CONTACT_COUNT: u32 = usage(0x0d, 0x54);
const DG_CONTACT_MAX: u32 = usage(0x0d, 0x55);
const DG_SCAN_TIME: u32 = usage(0x0d, 0x56);
const DG_SURFACE_SWITCH: u32 = usage(0x0d, 0x57);
const DG_BUTTON_SWITCH: u32 = usage(0x0d, 0x58);
const DG_BUTTON_TYPE: u32 = usage(0x0d, 0x59);
const DG_LATENCY_MODE: u32 = usage(0x0d, 0x60);
const GD_X: u32 = usage(0x01, 0x30);
const GD_Y: u32 = usage(0x01, 0x31);
/// The certification status blob (`mt_feature_mapping`'s `0xff0000c5`).
const CERTIFICATION_BLOB: u32 = 0xff00_00c5;

/// `MT_INPUTMODE_TOUCHPAD`: what `mt_set_modes` writes to Input Mode for a Touch Pad application.
pub const INPUT_MODE_TOUCHPAD: u32 = 0x03;
/// `MT_BUTTONTYPE_CLICKPAD` and `MT_BUTTONTYPE_PRESSUREPAD` (the Pad Type feature).
pub const BUTTON_TYPE_CLICKPAD: u32 = 0;
pub const BUTTON_TYPE_PRESSUREPAD: u32 = 1;
/// `MT_DEFAULT_MAXCONTACT`, `MT_MAX_MAXCONTACT`.
const DEFAULT_MAX_CONTACTS: u32 = 10;
const MAX_MAX_CONTACTS: i32 = 250;
/// Contacts per input report kept (a parallel-mode pad sends up to five; hybrid mode sends one).
pub const MAX_FINGERS_PER_REPORT: usize = 5;
/// Slots kept: [`DEFAULT_MAX_CONTACTS`], Linux's count for a pad that does not say.
pub const MAX_SLOTS: usize = tp_rules::SLOTS;
/// Feature reports kept.
const MAX_FEATURE_REPORTS: usize = 8;
/// Most bytes of a feature report this module builds (the mode reports are one byte each).
pub const MAX_MODE_REPORT: usize = 8;
/// `MAX_TIMESTAMP_INTERVAL` of hid-multitouch.c: after a second without a report the timestamp starts again from 0.
const MAX_TIMESTAMP_INTERVAL_US: u64 = 1_000_000;
/// `mt_touch_report`'s sticky-finger timer (`MT_QUIRK_STICKY_FINGERS`): contacts still down 100 ms after the last
/// report are released.
pub const STICKY_RELEASE_US: u64 = 100_000;
/// `TRKID_MAX` (`include/linux/input/mt.h`) and `TRKID_SGN` (`drivers/input/input-mt.c`).
const TRKID_MAX: i32 = 0xffff;
const TRKID_SGN: i32 = (TRKID_MAX + 1) >> 1;

/// An absolute axis of the pad: where it is in a report, its logical range and the resolution Linux gives it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Axis {
    pub field: Scalar,
    pub min: i32,
    pub max: i32,
    /// Units per millimetre, as `hidinput_calc_abs_res` computes it (0 if the descriptor does not say).
    pub resolution: i32,
}

/// One contact's fields in an input report (a Finger logical collection).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FingerFields {
    pub tip: Option<Scalar>,
    pub in_range: Option<Scalar>,
    pub confidence: Option<Scalar>,
    pub contact_id: Option<Scalar>,
    pub x: Option<Axis>,
    pub y: Option<Axis>,
}

/// A field of a feature report.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FeatureField {
    pub report_id: u8,
    pub bit_offset: u32,
    pub size: u32,
    pub logical_max: i32,
}

/// The Touch Pad application of a report descriptor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PtpFormat {
    /// The touch report's ID (0 if the device does not use IDs).
    pub report_id: u8,
    fingers: [FingerFields; MAX_FINGERS_PER_REPORT],
    nfingers: usize,
    pub x: Axis,
    pub y: Axis,
    pub scan_time: Option<Scalar>,
    pub scan_time_max: i32,
    pub contact_count: Option<Scalar>,
    /// Button usages 1.. of the touch report: bit offset of each, by usage - 1.
    buttons: [Option<u32>; 3],
    /// How many Button page usages the Touch Pad application has (`app->buttons_count`).
    pub buttons_count: u32,
    pub input_mode: Option<FeatureField>,
    pub surface_switch: Option<FeatureField>,
    pub button_switch: Option<FeatureField>,
    pub latency: Option<FeatureField>,
    pub contact_max: Option<FeatureField>,
    pub button_type: Option<FeatureField>,
    /// The report holding the certification blob.
    pub blob_report: Option<u8>,
    /// Feature reports in the order their IDs first appear, with their sizes in bits (without the ID).
    features: [(u8, u32); MAX_FEATURE_REPORTS],
    nfeatures: usize,
}

/// `enum latency_mode` of hid-multitouch.c.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Latency {
    Normal = 0,
    High = 1,
}

/// `enum report_mode` of hid-multitouch.c: what Surface Switch (contacts) and Button Switch report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReportMode {
    pub contacts: bool,
    pub buttons: bool,
}

impl ReportMode {
    pub const ALL: ReportMode = ReportMode {
        contacts: true,
        buttons: true,
    };
    pub const NONE: ReportMode = ReportMode {
        contacts: false,
        buttons: false,
    };
}

/// One SET_REPORT: a feature report's ID and bytes (without the ID).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModeReport {
    pub report_id: u8,
    data: [u8; MAX_MODE_REPORT],
    len: usize,
}

impl ModeReport {
    pub fn data(&self) -> &[u8] {
        &self.data[..self.len]
    }
}

/// One GET_REPORT the probe makes: a feature report's ID and its length with the ID byte (`hid_report_len`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProbeRead {
    pub report_id: u8,
    pub len: usize,
}

/// `hidinput_calc_abs_res` for a position axis (ABS_MT_POSITION_X/Y): units per mm, 0 if the units are not centimetres
/// or inches or the extents are not sane.
fn position_resolution(item: &InputItem) -> i32 {
    let mut unit_exponent = item.unit_exponent;
    let mut logical: i32 = item.logical_max.wrapping_sub(item.logical_min);
    let mut physical: i32 = item.physical_max.wrapping_sub(item.physical_min);
    if logical <= 0 || physical <= 0 {
        return 0;
    }
    match item.unit {
        // Centimetres.
        0x11 => unit_exponent += 1,
        // Inches.
        0x13 => {
            let Some(p) = physical.checked_mul(254) else {
                return 0;
            };
            physical = p;
            unit_exponent -= 1;
        }
        _ => return 0,
    }
    while unit_exponent < 0 {
        let Some(l) = logical.checked_mul(10) else {
            return 0;
        };
        logical = l;
        unit_exponent += 1;
    }
    while unit_exponent > 0 {
        let Some(p) = physical.checked_mul(10) else {
            return 0;
        };
        physical = p;
        unit_exponent -= 1;
    }
    // DIV_ROUND_CLOSEST for positive values.
    (logical + physical / 2) / physical
}

impl PtpFormat {
    /// The Touch Pad application of a report descriptor, or None if it has none with a contact's tip, X and Y.
    pub fn parse(desc: &[u8]) -> Option<PtpFormat> {
        let mut f = PtpFormat {
            report_id: 0,
            fingers: [FingerFields::default(); MAX_FINGERS_PER_REPORT],
            nfingers: 0,
            x: Axis::default(),
            y: Axis::default(),
            scan_time: None,
            scan_time_max: 0,
            contact_count: None,
            buttons: [None; 3],
            buttons_count: 0,
            input_mode: None,
            surface_switch: None,
            button_switch: None,
            latency: None,
            contact_max: None,
            button_type: None,
            blob_report: None,
            features: [(0, 0); MAX_FEATURE_REPORTS],
            nfeatures: 0,
        };
        let mut touch_report = None::<u8>;
        // The Logical collection the finger being filled is in.
        let mut finger_logical = [0u32; MAX_FINGERS_PER_REPORT];
        walk_items(desc, |item| {
            if item.kind == MAIN_FEATURE {
                f.feature_item(item);
                return;
            }
            if item.kind != MAIN_INPUT || item.application != DG_TOUCH_PAD {
                return;
            }
            if touch_report.is_some_and(|id| id != item.report_id) {
                return;
            }
            // hid-core's hid_add_field drops an item without usages (padding), so hid-multitouch never counts it.
            let has_usages = item.nusages > 0 || item.usage_min.is_some();
            for k in 0..item.count {
                let u = item.usage(k);
                if has_usages && u >> 16 == 0x09 {
                    f.buttons_count += 1;
                }
                if item.constant || item.size == 0 {
                    continue;
                }
                let scalar = Scalar {
                    bit_offset: item.bit_offset + k * item.size,
                    size: item.size,
                    signed: item.logical_min < 0,
                };
                touch_report.get_or_insert(item.report_id);
                match u {
                    DG_SCAN_TIME => {
                        f.scan_time = Some(scalar);
                        f.scan_time_max = item.logical_max;
                    }
                    DG_CONTACT_COUNT => f.contact_count = Some(scalar),
                    u if u >> 16 == 0x09 && item.variable => {
                        let n = (u & 0xffff) as usize;
                        if (1..=f.buttons.len()).contains(&n) {
                            f.buttons[n - 1] = Some(scalar.bit_offset);
                        }
                    }
                    DG_TIP_SWITCH | DG_IN_RANGE | DG_CONFIDENCE | DG_CONTACT_ID | GD_X | GD_Y => {
                        let Some(i) =
                            finger_index(&mut finger_logical, &mut f.nfingers, item.logical)
                        else {
                            continue;
                        };
                        let finger = &mut f.fingers[i];
                        let axis = Axis {
                            field: scalar,
                            min: item.logical_min,
                            max: item.logical_max,
                            resolution: position_resolution(item),
                        };
                        match u {
                            DG_TIP_SWITCH => finger.tip = Some(scalar),
                            DG_IN_RANGE => finger.in_range = Some(scalar),
                            DG_CONFIDENCE => finger.confidence = Some(scalar),
                            DG_CONTACT_ID => finger.contact_id = Some(scalar),
                            GD_X if finger.x.is_none() => finger.x = Some(axis),
                            GD_Y if finger.y.is_none() => finger.y = Some(axis),
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
        })?;
        f.report_id = touch_report?;
        let first = *f.fingers.first().filter(|_| f.nfingers > 0)?;
        first.tip?;
        f.x = first.x?;
        f.y = first.y?;
        Some(f)
    }

    fn feature_item(&mut self, item: &InputItem) {
        let index = match self.features[..self.nfeatures]
            .iter()
            .position(|(id, _)| *id == item.report_id)
        {
            Some(i) => i,
            None if self.nfeatures < MAX_FEATURE_REPORTS => {
                self.features[self.nfeatures] = (item.report_id, 0);
                self.nfeatures += 1;
                self.nfeatures - 1
            }
            None => return,
        };
        self.features[index].1 = self.features[index]
            .1
            .max(item.bit_offset + item.size * item.count);
        if item.constant || item.size == 0 {
            return;
        }
        for k in 0..item.count {
            let field = FeatureField {
                report_id: item.report_id,
                bit_offset: item.bit_offset + k * item.size,
                size: item.size,
                logical_max: item.logical_max,
            };
            let slot = match item.usage(k) {
                // `mt_need_to_apply_feature` takes only the first Input Mode ("some elan panels wrongly declare 2").
                DG_INPUT_MODE => &mut self.input_mode,
                DG_SURFACE_SWITCH => &mut self.surface_switch,
                DG_BUTTON_SWITCH => &mut self.button_switch,
                DG_LATENCY_MODE => &mut self.latency,
                DG_CONTACT_MAX => &mut self.contact_max,
                DG_BUTTON_TYPE => &mut self.button_type,
                CERTIFICATION_BLOB if k == 0 => {
                    self.blob_report.get_or_insert(item.report_id);
                    continue;
                }
                _ => continue,
            };
            slot.get_or_insert(field);
        }
    }

    /// The fields of each contact an input report carries (one in hybrid mode).
    pub fn fingers(&self) -> &[FingerFields] {
        &self.fingers[..self.nfingers]
    }

    /// A feature report's length in bytes, with its ID byte if it has one (`hid_report_len`).
    pub fn feature_len(&self, report_id: u8) -> Option<usize> {
        self.features[..self.nfeatures]
            .iter()
            .find(|(id, _)| *id == report_id)
            .map(|(id, bits)| bits.div_ceil(8) as usize + usize::from(*id > 0))
    }

    /// The GET_REPORTs `mt_feature_mapping` makes when hid-input maps the feature usages, in descriptor order: the report
    /// with Contact Count Maximum, the one with Pad Type (`HID_DG_BUTTONTYPE`), and the certification blob's report
    /// ("Retrieve the Win8 blob once to enable some devices"). Linux fetches the first report once per usage it holds;
    /// one fetch of each report is kept here, since the second answer of the same report replaces the first.
    pub fn probe_reads(&self) -> impl Iterator<Item = ProbeRead> + '_ {
        let mut seen = [0u8; 3];
        let mut n = 0;
        let mut out = [None::<ProbeRead>; 3];
        for id in [
            self.contact_max.map(|f| f.report_id),
            self.button_type.map(|f| f.report_id),
            self.blob_report,
        ]
        .into_iter()
        .flatten()
        {
            if seen[..n].contains(&id) {
                continue;
            }
            if let Some(len) = self.feature_len(id) {
                seen[n] = id;
                out[n] = Some(ProbeRead { report_id: id, len });
                n += 1;
            }
        }
        out.into_iter().flatten()
    }

    /// `mt_set_modes(latency, report_mode)`: each feature report in the order its ID first appears in the descriptor, sent
    /// whole (every other field 0, as Linux's report values start) if `mt_need_to_apply_feature` set a field in it:
    /// Input Mode to [`INPUT_MODE_TOUCHPAD`], Latency Mode, Surface Switch to `report.contacts`, Button Switch to
    /// `report.buttons`. Contact Count Maximum is not written (`MT_CLS_WIN_8` gives no `maxcontacts`).
    pub fn mode_reports(
        &self,
        latency: Latency,
        report: ReportMode,
    ) -> impl Iterator<Item = ModeReport> + '_ {
        let values = [
            (self.input_mode, INPUT_MODE_TOUCHPAD),
            (self.latency, latency as u32),
            (self.surface_switch, u32::from(report.contacts)),
            (self.button_switch, u32::from(report.buttons)),
        ];
        self.features[..self.nfeatures]
            .iter()
            .filter_map(move |&(id, bits)| {
                let len = bits.div_ceil(8) as usize;
                if len > MAX_MODE_REPORT {
                    return None;
                }
                let mut m = ModeReport {
                    report_id: id,
                    data: [0; MAX_MODE_REPORT],
                    len,
                };
                let mut any = false;
                for (field, value) in values {
                    if let Some(field) = field.filter(|f| f.report_id == id) {
                        put_bits(&mut m.data[..len], field.bit_offset, field.size, value);
                        any = true;
                    }
                }
                any.then_some(m)
            })
    }

    /// The maximum number of contacts, as `mt_feature_mapping` and `mt_touch_input_configured` settle it: the Contact
    /// Count Maximum the device returned (`ccm`, None if it was not read), else that field's logical maximum if it is at
    /// most 250, else 10. Kept to [`MAX_SLOTS`].
    pub fn max_contacts(&self, ccm: Option<u32>) -> usize {
        let from_feature = ccm.filter(|&v| v != 0).or_else(|| {
            self.contact_max
                .map(|f| f.logical_max)
                .filter(|&m| m > 0 && m <= MAX_MAX_CONTACTS)
                .map(|m| m as u32)
        });
        (from_feature.unwrap_or(DEFAULT_MAX_CONTACTS) as usize).min(MAX_SLOTS)
    }

    /// Reads a feature field out of a feature report as GET_REPORT returned it (its ID first if it has one).
    pub fn read_feature(&self, field: FeatureField, report: &[u8]) -> Option<u32> {
        let payload = if field.report_id != 0 {
            report
                .split_first()
                .filter(|(id, _)| **id == field.report_id)?
                .1
        } else {
            report
        };
        bits(payload, field.bit_offset, field.size)
    }

    /// Whether `report` (its ID first if the pad uses IDs) is the touch report.
    pub fn is_touch_report(&self, report: &[u8]) -> bool {
        self.report_id == 0 || report.first() == Some(&self.report_id)
    }
}

/// The finger slot for an item in Logical collection `logical`: the one already filled from it, else the next.
fn finger_index(
    logical: &mut [u32; MAX_FINGERS_PER_REPORT],
    n: &mut usize,
    of: u32,
) -> Option<usize> {
    if let Some(i) = logical[..*n].iter().position(|&l| l == of) {
        return Some(i);
    }
    if *n == logical.len() {
        return None;
    }
    logical[*n] = of;
    *n += 1;
    Some(*n - 1)
}

/// Writes the low `size` bits of `value` at bit `offset` of `data`, least significant bit first.
fn put_bits(data: &mut [u8], offset: u32, size: u32, value: u32) {
    for k in 0..size.min(32) {
        let bit = offset + k;
        if let Some(byte) = data.get_mut((bit / 8) as usize) {
            let mask = 1u8 << (bit % 8);
            if (value >> k) & 1 != 0 {
                *byte |= mask;
            } else {
                *byte &= !mask;
            }
        }
    }
}

/// `MT_TOOL_FINGER`, `MT_TOOL_PALM` (`include/uapi/linux/input.h`).
pub const MT_TOOL_FINGER: i32 = 0;
pub const MT_TOOL_PALM: i32 = 2;

/// A slot after a frame: what `input_mt` holds for it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Slot {
    /// ABS_MT_TRACKING_ID: -1 when the slot is free.
    pub tracking_id: i32,
    /// The pad's contact ID this slot is keyed by (`input_mt_slot.key`).
    pub key: i32,
    pub tool: i32,
    pub x: i32,
    pub y: i32,
}

impl Slot {
    const FREE: Slot = Slot {
        tracking_id: -1,
        key: 0,
        tool: MT_TOOL_FINGER,
        x: 0,
        y: 0,
    };

    pub fn active(&self) -> bool {
        self.tracking_id >= 0
    }
}

/// A whole frame, as evdev readers see it at SYN_REPORT.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Frame {
    /// MSC_TIMESTAMP: µs since the scan times started counting (`mt_compute_timestamp`).
    pub timestamp_us: u64,
    pub slots: [Slot; MAX_SLOTS],
    pub num_slots: usize,
    /// BTN_LEFT (`MT_QUIRK_WIN8_PTP_BUTTONS`: every button of the pad is the left one).
    pub left: bool,
}

impl Frame {
    /// Active slots, palms included.
    pub fn count(&self) -> usize {
        self.slots[..self.num_slots]
            .iter()
            .filter(|s| s.active())
            .count()
    }
}

/// One evdev event of the raw multi-touch device: type, code, value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MtEvent {
    pub kind: u16,
    pub code: u16,
    pub value: i32,
}

pub const EV_SYN: u16 = 0x00;
pub const EV_KEY: u16 = 0x01;
pub const EV_ABS: u16 = 0x03;
pub const SYN_REPORT: u16 = 0;
pub const ABS_X: u16 = 0x00;
pub const ABS_Y: u16 = 0x01;
pub const ABS_MT_SLOT: u16 = 0x2f;
pub const ABS_MT_TOOL_TYPE: u16 = 0x37;
pub const ABS_MT_POSITION_X: u16 = 0x35;
pub const ABS_MT_POSITION_Y: u16 = 0x36;
pub const ABS_MT_TRACKING_ID: u16 = 0x39;
pub const BTN_LEFT: u16 = 0x110;
pub const BTN_TOOL_FINGER: u16 = 0x145;
pub const BTN_TOUCH: u16 = 0x14a;
pub const BTN_TOOL_DOUBLETAP: u16 = 0x14d;
pub const BTN_TOOL_TRIPLETAP: u16 = 0x14e;
pub const BTN_TOOL_QUADTAP: u16 = 0x14f;
pub const BTN_TOOL_QUINTTAP: u16 = 0x148;

/// The contact tracker: hid-multitouch's touch path for one Touch Pad application, and input-mt's slots.
pub struct Contacts {
    format: PtpFormat,
    num_slots: usize,
    slots: [Slot; MAX_SLOTS],
    /// The frame number each slot was last reported in (`input_mt_slot.frame`), and the current frame (`mt->frame`).
    slot_frame: [u32; MAX_SLOTS],
    frame: u32,
    trkid: i32,
    pending_palm: [bool; MAX_SLOTS],
    num_received: u32,
    num_expected: u32,
    prev_scantime: Option<u32>,
    timestamp_us: u64,
    last_report_us: Option<u64>,
    left_button: bool,
    /// The last value of each event code sent, so that, as Linux's input core does, a value that did not change is not
    /// sent again (`input_handle_abs_event` / `input_get_disposition`): per slot for the MT codes. ABS_MT_SLOT is staged
    /// and only sent before a slot value that changed, when it differs from the one sent last.
    staged_slot: i32,
    sent_slot: i32,
    sent_mt: [[Option<i32>; 4]; MAX_SLOTS],
    sent_keys: [Option<i32>; 8],
    sent_abs: [Option<i32>; 2],
}

/// The keys [`Contacts`] sends: BTN_LEFT, BTN_TOUCH, then the five tool keys.
const KEYS: [u16; 7] = [
    BTN_LEFT,
    BTN_TOUCH,
    BTN_TOOL_FINGER,
    BTN_TOOL_DOUBLETAP,
    BTN_TOOL_TRIPLETAP,
    BTN_TOOL_QUADTAP,
    BTN_TOOL_QUINTTAP,
];

impl Contacts {
    /// `max_contacts` as [`PtpFormat::max_contacts`] gives it.
    pub fn new(format: PtpFormat, max_contacts: usize) -> Contacts {
        Contacts {
            format,
            num_slots: max_contacts.clamp(1, MAX_SLOTS),
            slots: [Slot::FREE; MAX_SLOTS],
            slot_frame: [u32::MAX; MAX_SLOTS],
            frame: 0,
            trkid: 0,
            pending_palm: [false; MAX_SLOTS],
            num_received: 0,
            num_expected: 0,
            prev_scantime: None,
            timestamp_us: 0,
            last_report_us: None,
            left_button: false,
            // input_mt_init_slots: every slot's tracking ID -1, the rest 0, slot 0.
            staged_slot: 0,
            sent_slot: 0,
            sent_mt: [[Some(-1), Some(0), Some(0), Some(0)]; MAX_SLOTS],
            sent_keys: [Some(0); 8],
            sent_abs: [Some(0); 2],
        }
    }

    pub fn format(&self) -> &PtpFormat {
        &self.format
    }

    pub fn num_slots(&self) -> usize {
        self.num_slots
    }

    /// Whether a contact is down (the sticky-finger timer of `mt_touch_report` runs while one is).
    pub fn any_active(&self) -> bool {
        self.slots[..self.num_slots].iter().any(|s| s.active())
    }

    /// When the sticky-finger release is due, if a contact is down.
    pub fn sticky_deadline(&self) -> Option<u64> {
        self.last_report_us
            .filter(|_| self.any_active())
            .map(|t| t + STICKY_RELEASE_US)
    }

    /// `mt_touch_report` for one input report (its ID first if the pad uses IDs), read at `now_us`. Calls `frame` with
    /// each frame that ends here (`mt_sync_frame`, and `mt_release_pending_palms`' second frame) and `event` with the
    /// evdev events of the raw device. Reports of other IDs are ignored.
    pub fn report(
        &mut self,
        report: &[u8],
        now_us: u64,
        frame: &mut dyn FnMut(&Frame),
        event: &mut dyn FnMut(MtEvent),
    ) {
        if !self.format.is_touch_report(report) {
            return;
        }
        let payload = if self.format.report_id != 0 {
            &report[1..]
        } else {
            report
        };
        let f = self.format;
        let read = |s: Option<Scalar>| s.and_then(|s| s.read(payload));
        let scantime = read(f.scan_time).map(|v| v as u32).unwrap_or(0);
        self.compute_timestamp(scantime, now_us);
        self.last_report_us = Some(now_us);
        let contact_count = read(f.contact_count);
        if let Some(cc) = contact_count.map(|c| c.max(0) as u32) {
            // MT_QUIRK_WIN8_PTP_BUTTONS: a first packet may carry only a button with a count of 0; it is a first packet if
            // the scan time changed. A non-zero count always starts a frame.
            self.num_expected = expected_contacts(
                self.num_received,
                self.prev_scantime != Some(scantime),
                cc,
                self.num_expected,
            );
        }
        self.prev_scantime = Some(scantime);
        let first_packet = self.num_received == 0;
        for finger in f.fingers() {
            if self.process_slot(finger, payload, event) {
                self.num_received += 1;
            }
        }
        // The buttons only in the first packet of a frame, ORed into BTN_LEFT.
        if first_packet {
            for offset in f.buttons.iter().flatten() {
                self.left_button |= bits(payload, *offset, 1) == Some(1);
            }
        }
        // `mt_post_parse`: without a Contact Count field MT_QUIRK_CONTACT_CNT_ACCURATE is dropped and every report ends a
        // frame (num_expected stays 0).
        if frame_ends(self.num_received, self.num_expected) {
            self.sync_frame(frame, event);
        }
    }

    /// `mt_compute_timestamp`: scan time deltas in 100 µs, wrapping at the field's logical maximum, from 0 again after a
    /// second without reports.
    fn compute_timestamp(&mut self, scantime: u32, now_us: u64) {
        let resync = self
            .last_report_us
            .is_none_or(|t| now_us.saturating_sub(t) > MAX_TIMESTAMP_INTERVAL_US);
        let prev = self.prev_scantime.unwrap_or(scantime);
        let mut delta = i64::from(scantime) - i64::from(prev);
        if delta < 0 {
            delta += i64::from(self.format.scan_time_max);
        }
        self.timestamp_us = if resync {
            0
        } else {
            self.timestamp_us + (delta.max(0) as u64) * 100
        };
    }

    /// `mt_process_slot` with MT_QUIRK_ALWAYS_VALID, _IGNORE_DUPLICATES, _HOVERING, _CONTACT_CNT_ACCURATE and
    /// _CONFIDENCE (the Win8 class on a Touch Pad application). Returns whether the contact counts as received.
    fn process_slot(
        &mut self,
        finger: &FingerFields,
        payload: &[u8],
        event: &mut dyn FnMut(MtEvent),
    ) -> bool {
        if !takes_contact(
            self.format.contact_count.is_some(),
            self.num_received,
            self.num_expected,
        ) {
            return false;
        }
        let read = |s: Option<Scalar>| s.and_then(|s| s.read(payload));
        let key = read(finger.contact_id).unwrap_or(0);
        let Some(n) = self.slot_by_key(key) else {
            return true;
        };
        if self.slots[n].active() && self.slot_frame[n] == self.frame {
            return false;
        }
        // A field the pad does not have reads as hid-multitouch's defaults: confidence true, in range false.
        let confidence = read(finger.confidence).is_none_or(|v| v != 0);
        let in_range = read(finger.in_range).is_some_and(|v| v != 0);
        let tip = read(finger.tip).is_some_and(|v| v != 0);
        let mut active = tip || in_range;
        let mut tool = MT_TOOL_FINGER;
        if !confidence {
            tool = MT_TOOL_PALM;
            if !active && self.slots[n].active() {
                // A contact that lost confidence as it lifted: first an active palm, then the lift after the frame.
                active = true;
                self.pending_palm[n] = true;
            }
        }
        self.set_slot(n, event);
        self.report_slot_state(n, tool, active, event);
        if active {
            let x = finger.x.and_then(|a| a.field.read(payload)).unwrap_or(0);
            let y = finger.y.and_then(|a| a.field.read(payload)).unwrap_or(0);
            self.slots[n].x = x;
            self.slots[n].y = y;
            self.send_mt(n, 2, ABS_MT_POSITION_X, x, event);
            self.send_mt(n, 3, ABS_MT_POSITION_Y, y, event);
        }
        true
    }

    /// `input_mt_get_slot_by_key`: the active slot with this key, else the first free one not used in this frame.
    fn slot_by_key(&mut self, key: i32) -> Option<usize> {
        let active = self.slots.map(|s| s.active());
        let keys = self.slots.map(|s| s.key);
        let frame = self.frame;
        let used = self.slot_frame.map(|f| f == frame);
        let i = tp_rules::slot_by_key(&active, &keys, &used, self.num_slots, key)?;
        self.slots[i].key = key;
        Some(i)
    }

    /// `input_mt_report_slot_state`.
    fn report_slot_state(
        &mut self,
        n: usize,
        tool: i32,
        active: bool,
        event: &mut dyn FnMut(MtEvent),
    ) {
        self.slot_frame[n] = self.frame;
        if !active {
            self.slots[n].tracking_id = -1;
            self.send_mt(n, 0, ABS_MT_TRACKING_ID, -1, event);
            return;
        }
        if self.slots[n].tracking_id < 0 {
            // input_mt_new_trkid.
            self.slots[n].tracking_id = self.trkid & TRKID_MAX;
            self.trkid = self.trkid.wrapping_add(1);
        }
        self.slots[n].tool = tool;
        let id = self.slots[n].tracking_id;
        self.send_mt(n, 0, ABS_MT_TRACKING_ID, id, event);
        self.send_mt(n, 1, ABS_MT_TOOL_TYPE, tool, event);
    }

    fn set_slot(&mut self, n: usize, _event: &mut dyn FnMut(MtEvent)) {
        self.staged_slot = n as i32;
    }

    fn send_mt(
        &mut self,
        n: usize,
        which: usize,
        code: u16,
        value: i32,
        event: &mut dyn FnMut(MtEvent),
    ) {
        if self.sent_mt[n][which] != Some(value) {
            if self.sent_slot != self.staged_slot {
                self.sent_slot = self.staged_slot;
                event(MtEvent {
                    kind: EV_ABS,
                    code: ABS_MT_SLOT,
                    value: self.staged_slot,
                });
            }
            self.sent_mt[n][which] = Some(value);
            event(MtEvent {
                kind: EV_ABS,
                code,
                value,
            });
        }
    }

    fn send_key(&mut self, which: usize, value: bool, event: &mut dyn FnMut(MtEvent)) {
        let value = i32::from(value);
        if self.sent_keys[which] != Some(value) {
            self.sent_keys[which] = Some(value);
            event(MtEvent {
                kind: EV_KEY,
                code: KEYS[which],
                value,
            });
        }
    }

    /// `mt_sync_frame`: BTN_LEFT, `input_mt_sync_frame` (pointer emulation with the finger count, as INPUT_MT_POINTER),
    /// SYN_REPORT; then `mt_release_pending_palms`, which lifts the palms of this frame in a frame of their own.
    fn sync_frame(&mut self, frame: &mut dyn FnMut(&Frame), event: &mut dyn FnMut(MtEvent)) {
        let left = self.left_button;
        self.send_key(0, left, event);
        self.end_frame(frame, event);
        let mut need_sync = false;
        for n in 0..self.num_slots {
            if core::mem::take(&mut self.pending_palm[n]) {
                self.set_slot(n, event);
                self.slots[n].tracking_id = -1;
                self.send_mt(n, 0, ABS_MT_TRACKING_ID, -1, event);
                need_sync = true;
            }
        }
        if need_sync {
            self.end_frame(frame, event);
        }
        self.num_received = 0;
        self.left_button = false;
    }

    /// `input_mt_sync_frame` (pointer emulation) and SYN_REPORT, and the frame for the gesture code.
    fn end_frame(&mut self, frame: &mut dyn FnMut(&Frame), event: &mut dyn FnMut(MtEvent)) {
        let mut oldest = None::<usize>;
        let mut oldid = self.trkid;
        let mut count = 0;
        for n in 0..self.num_slots {
            let id = self.slots[n].tracking_id;
            if id < 0 {
                continue;
            }
            if (id - oldid) & TRKID_SGN != 0 {
                oldest = Some(n);
                oldid = id;
            }
            count += 1;
        }
        self.send_key(1, count > 0, event);
        // input_mt_report_finger_count.
        for (i, n) in [1, 2, 3, 4, 5].into_iter().enumerate() {
            self.send_key(2 + i, count == n, event);
        }
        if let Some(n) = oldest {
            let (x, y) = (self.slots[n].x, self.slots[n].y);
            for (i, (code, v)) in [(ABS_X, x), (ABS_Y, y)].into_iter().enumerate() {
                if self.sent_abs[i] != Some(v) {
                    self.sent_abs[i] = Some(v);
                    event(MtEvent {
                        kind: EV_ABS,
                        code,
                        value: v,
                    });
                }
            }
        }
        event(MtEvent {
            kind: EV_SYN,
            code: SYN_REPORT,
            value: 0,
        });
        self.frame = self.frame.wrapping_add(1);
        frame(&Frame {
            timestamp_us: self.timestamp_us,
            slots: self.slots,
            num_slots: self.num_slots,
            left: self.left_button,
        });
    }

    /// `mt_release_contacts`, from the sticky-finger timer (`mt_expired_timeout`) or a reset: every slot lifted in one
    /// frame, and the frame being built forgotten. Does nothing if no contact is down when called from the timer.
    pub fn release_all(&mut self, frame: &mut dyn FnMut(&Frame), event: &mut dyn FnMut(MtEvent)) {
        for n in 0..self.num_slots {
            self.set_slot(n, event);
            self.slot_frame[n] = self.frame;
            self.slots[n].tracking_id = -1;
            self.send_mt(n, 0, ABS_MT_TRACKING_ID, -1, event);
        }
        self.left_button = false;
        self.end_frame(frame, event);
        self.num_received = 0;
    }

    /// The sticky-finger timer at `now_us`: releases the contacts if no report came for [`STICKY_RELEASE_US`].
    pub fn tick(
        &mut self,
        now_us: u64,
        frame: &mut dyn FnMut(&Frame),
        event: &mut dyn FnMut(MtEvent),
    ) {
        if self.sticky_deadline().is_some_and(|d| now_us >= d) {
            self.release_all(frame, event);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;
