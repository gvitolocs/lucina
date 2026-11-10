//! The video probe and commit controls (UVC 1.1 section 4.3.1.1): the fields, their wire layout, the requests that carry them,
//! and Linux's corrections of what a camera answers.
//!
//! After Linux `uvc_video.c` (v6.16): `uvc_video_ctrl_size` (26 bytes before UVC 1.1, 34 before 1.5, 48 from 1.5),
//! `uvc_get_video_ctrl`, `uvc_set_video_ctrl`, `uvc_fixup_video_ctrl` (the generic parts; this camera has no quirk entry),
//! `uvc_probe_video`, and `uvc_v4l2.c` `uvc_try_frame_interval`. The probe sequence itself (set, GET_MIN, GET_MAX, then up to
//! two set/get rounds while the stream needs more bandwidth than any alternate setting gives, then the commit) is [`Probe`],
//! so the kernel only moves the bytes.

use alloc::vec::Vec;

use crate::desc::{Format, Frame, Intervals, Streaming};

/// Class-specific requests (UVC table A-8).
pub const SET_CUR: u8 = 0x01;
pub const GET_CUR: u8 = 0x81;
pub const GET_MIN: u8 = 0x82;
pub const GET_MAX: u8 = 0x83;
pub const GET_DEF: u8 = 0x87;
/// VideoStreaming control selectors (table A-15), sent in the high byte of `wValue`.
pub const VS_PROBE_CONTROL: u8 = 0x01;
pub const VS_COMMIT_CONTROL: u8 = 0x02;

/// `bmHint` bit 0: keep `dwFrameInterval`.
pub const HINT_FRAME_INTERVAL: u16 = 1;

/// `uvc_video_ctrl_size`.
pub fn control_size(uvc_version: u16) -> usize {
    if uvc_version < 0x0110 {
        26
    } else if uvc_version < 0x0150 {
        34
    } else {
        48
    }
}

/// `struct uvc_streaming_control`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Control {
    pub hint: u16,
    pub format_index: u8,
    pub frame_index: u8,
    pub frame_interval: u32,
    pub key_frame_rate: u16,
    pub p_frame_rate: u16,
    pub comp_quality: u16,
    pub comp_window_size: u16,
    pub delay: u16,
    pub max_video_frame_size: u32,
    pub max_payload_transfer_size: u32,
    pub clock_frequency: u32,
    pub framing_info: u8,
    pub preferred_version: u8,
    pub min_version: u8,
    pub max_version: u8,
}

impl Control {
    /// The request for a format, a frame and an interval, as `uvc_v4l2_try_format` builds it: everything else zero.
    pub fn request(format: &Format, frame: &Frame, interval: u32) -> Control {
        Control {
            hint: HINT_FRAME_INTERVAL,
            format_index: format.index,
            frame_index: frame.index,
            frame_interval: try_frame_interval(frame, interval),
            ..Control::default()
        }
    }

    /// `uvc_set_video_ctrl`: the bytes of a SET_CUR, `size` long. From 34 bytes on, the UVC 1.1 fields follow; Linux writes
    /// nothing beyond them (the 1.5 fields stay zero).
    pub fn encode(&self, size: usize) -> Vec<u8> {
        let mut d = alloc::vec![0u8; size];
        d[0..2].copy_from_slice(&self.hint.to_le_bytes());
        d[2] = self.format_index;
        d[3] = self.frame_index;
        d[4..8].copy_from_slice(&self.frame_interval.to_le_bytes());
        d[8..10].copy_from_slice(&self.key_frame_rate.to_le_bytes());
        d[10..12].copy_from_slice(&self.p_frame_rate.to_le_bytes());
        d[12..14].copy_from_slice(&self.comp_quality.to_le_bytes());
        d[14..16].copy_from_slice(&self.comp_window_size.to_le_bytes());
        d[16..18].copy_from_slice(&self.delay.to_le_bytes());
        d[18..22].copy_from_slice(&self.max_video_frame_size.to_le_bytes());
        d[22..26].copy_from_slice(&self.max_payload_transfer_size.to_le_bytes());
        if size >= 34 {
            d[26..30].copy_from_slice(&self.clock_frequency.to_le_bytes());
            d[30] = self.framing_info;
            d[31] = self.preferred_version;
            d[32] = self.min_version;
            d[33] = self.max_version;
        }
        d
    }

    /// `uvc_get_video_ctrl` on a full answer (`d.len() == size`): the fields, then `uvc_fixup_video_ctrl`. Before UVC 1.1 the
    /// clock frequency is the VideoControl header's.
    pub fn decode(d: &[u8], size: usize, uvc_version: u16, clock_frequency: u32, stream: &Streaming) -> Option<Control> {
        if d.len() != size || size < 26 {
            return None;
        }
        let u16_at = |i: usize| u16::from_le_bytes([d[i], d[i + 1]]);
        let u32_at = |i: usize| u32::from_le_bytes([d[i], d[i + 1], d[i + 2], d[i + 3]]);
        let mut c = Control {
            hint: u16_at(0),
            format_index: d[2],
            frame_index: d[3],
            frame_interval: u32_at(4),
            key_frame_rate: u16_at(8),
            p_frame_rate: u16_at(10),
            comp_quality: u16_at(12),
            comp_window_size: u16_at(14),
            delay: u16_at(16),
            max_video_frame_size: u32_at(18),
            max_payload_transfer_size: u32_at(22),
            clock_frequency,
            ..Control::default()
        };
        if size >= 34 {
            c.clock_frequency = u32_at(26);
            c.framing_info = d[30];
            c.preferred_version = d[31];
            c.min_version = d[32];
            c.max_version = d[33];
        }
        c.fix_up(uvc_version, stream);
        Some(c)
    }

    /// The generic part of `uvc_fixup_video_ctrl`: an uncompressed frame always takes its frame descriptor's buffer size, and
    /// so does a compressed one that answers 0 before UVC 1.1; a bandwidth whose top 16 bits are all ones lost them (a
    /// sign-extended 16-bit value, seen on Chicony 04f2:b50b).
    fn fix_up(&mut self, uvc_version: u16, stream: &Streaming) {
        let Some(format) = stream.format(self.format_index) else {
            return;
        };
        let Some(frame) = format.frame(self.frame_index) else {
            return;
        };
        if !format.compressed || (self.max_video_frame_size == 0 && uvc_version < 0x0110) {
            self.max_video_frame_size = frame.max_frame_size;
        }
        if self.max_payload_transfer_size & 0xffff_0000 == 0xffff_0000 {
            self.max_payload_transfer_size &= !0xffff_0000;
        }
    }
}

/// `uvc_try_frame_interval`: the supported interval nearest to `interval`. Discrete lists are walked in order and the walk stops
/// as soon as the distance grows, as Linux does. A continuous range is rounded to its step and capped at its maximum; below its
/// minimum it gives the minimum (Linux computes `interval - min` unsigned there, which wraps).
pub fn try_frame_interval(frame: &Frame, interval: u32) -> u32 {
    match &frame.intervals {
        Intervals::Discrete(list) if !list.is_empty() => {
            let mut best = u32::MAX;
            let mut i = 0;
            while i < list.len() {
                let dist = interval.abs_diff(list[i]);
                if dist > best {
                    break;
                }
                best = dist;
                i += 1;
            }
            list[i - 1]
        }
        Intervals::Discrete(_) => interval,
        Intervals::Continuous { min, max, step } => {
            let (min, max, step) = (*min, *max, (*step).max(1));
            let interval = interval.max(min);
            let rounded = min.saturating_add((interval - min).saturating_add(step / 2) / step * step);
            rounded.min(max)
        }
    }
}

/// A control request on the streaming interface: `request` (one of the consts above), `selector`, and for SET_CUR the bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transfer {
    pub request: u8,
    pub selector: u8,
    pub data: Option<Vec<u8>>,
}

/// The probe sequence of `uvc_probe_video` and the commit of `uvc_commit_video`, one control transfer at a time.
#[derive(Clone, Debug)]
pub struct Probe {
    size: usize,
    uvc_version: u16,
    clock_frequency: u32,
    /// The largest bandwidth an alternate setting gives, and whether there is no alternate setting but 0.
    max_bandwidth: u32,
    single_alt: bool,
    probe: Control,
    min: Control,
    max: Control,
    step: ProbeStep,
    round: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeStep {
    FirstSet,
    GetMin,
    GetMax,
    Set,
    GetCur,
    Commit,
    Done,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeError {
    /// An answer of the wrong length, or a request the camera refused.
    Failed(ProbeStep),
}

impl Probe {
    pub fn new(camera: &crate::desc::Camera, stream: &Streaming, request: Control) -> Probe {
        Probe {
            size: control_size(camera.uvc_version),
            uvc_version: camera.uvc_version,
            clock_frequency: camera.clock_frequency,
            max_bandwidth: stream.max_bytes_per_interval(),
            single_alt: stream.alts.is_empty(),
            probe: request,
            min: Control::default(),
            max: Control::default(),
            step: ProbeStep::FirstSet,
            round: 0,
        }
    }

    pub fn size(&self) -> usize {
        self.size
    }

    /// The transfer to make now, or `None` when the stream is committed.
    pub fn next(&self) -> Option<Transfer> {
        let set = |selector| Transfer { request: SET_CUR, selector, data: Some(self.probe.encode(self.size)) };
        let get = |request| Transfer { request, selector: VS_PROBE_CONTROL, data: None };
        match self.step {
            ProbeStep::FirstSet | ProbeStep::Set => Some(set(VS_PROBE_CONTROL)),
            ProbeStep::GetMin => Some(get(GET_MIN)),
            ProbeStep::GetMax => Some(get(GET_MAX)),
            ProbeStep::GetCur => Some(get(GET_CUR)),
            ProbeStep::Commit => Some(set(VS_COMMIT_CONTROL)),
            ProbeStep::Done => None,
        }
    }

    pub fn step(&self) -> ProbeStep {
        self.step
    }

    /// What the transfer of [`Probe::next`] gave: `Some(bytes)` for a GET, `Some(empty)` for a SET that went through, `None`
    /// for one that failed or stalled.
    pub fn answer(&mut self, stream: &Streaming, result: Option<&[u8]>) -> Result<(), ProbeError> {
        let step = self.step;
        let failed = Err(ProbeError::Failed(step));
        let Some(data) = result else {
            return failed;
        };
        let decode = |d: &[u8]| Control::decode(d, self.size, self.uvc_version, self.clock_frequency, stream);
        match step {
            ProbeStep::FirstSet => self.step = ProbeStep::GetMin,
            ProbeStep::GetMin | ProbeStep::GetMax => {
                // Linux: some cameras answer GET_MIN/GET_MAX with only wCompQuality (2 bytes).
                let control = if data.len() == 2 {
                    Control { comp_quality: u16::from_le_bytes([data[0], data[1]]), ..Control::default() }
                } else {
                    match decode(data) {
                        Some(c) => c,
                        None => return failed,
                    }
                };
                if step == ProbeStep::GetMin {
                    self.min = control;
                    self.step = ProbeStep::GetMax;
                } else {
                    self.max = control;
                    self.probe.comp_quality = self.max.comp_quality;
                    self.step = ProbeStep::Set;
                }
            }
            ProbeStep::Set => self.step = ProbeStep::GetCur,
            ProbeStep::GetCur => {
                let Some(c) = decode(data) else {
                    return failed;
                };
                self.probe = c;
                self.round += 1;
                if self.single_alt || self.probe.max_payload_transfer_size <= self.max_bandwidth || self.round >= 2 {
                    self.step = ProbeStep::Commit;
                } else {
                    // As Linux: the compression parameters to the camera's minimum (quality to its maximum), and one more round.
                    self.probe.key_frame_rate = self.min.key_frame_rate;
                    self.probe.p_frame_rate = self.min.p_frame_rate;
                    self.probe.comp_quality = self.max.comp_quality;
                    self.probe.comp_window_size = self.min.comp_window_size;
                    self.step = ProbeStep::Set;
                }
            }
            ProbeStep::Commit => self.step = ProbeStep::Done,
            ProbeStep::Done => {}
        }
        Ok(())
    }

    /// The control the camera agreed to (valid once [`ProbeStep::Commit`] or later is reached).
    pub fn control(&self) -> &Control {
        &self.probe
    }
}

/// `uvc_video_start_transfer` for isochronous streams: the alternate setting whose endpoint is the smallest that still moves
/// `bandwidth` bytes per service interval. `None` when none is big enough.
pub fn choose_alt(stream: &Streaming, bandwidth: u32) -> Option<crate::desc::AltSetting> {
    let mut best: Option<crate::desc::AltSetting> = None;
    let mut best_size = u32::MAX;
    for alt in &stream.alts {
        if crate::rules::better_alt(alt.bytes_per_interval, bandwidth, best_size) {
            best = Some(*alt);
            best_size = alt.bytes_per_interval;
        }
    }
    best
}
