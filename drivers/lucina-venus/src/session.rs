// SPDX-License-Identifier: GPL-2.0-only
//! A decode session's packets and messages: `pkt_session_*` of hfi_cmds.c for 4xx and the session messages of hfi_msgs.c, as words.
//! What a session sends and in which order is in [`crate::decode`]; `tests/oracle.rs` compares these with Linux's own packetizer.
//!
//! Every message read here comes from the firmware, which parsed a file someone else made: every field is checked before it is used,
//! and anything that does not fit is an error, not a guess (Linux reads an unknown property of the sequence-changed event as one with
//! no payload, and takes a fill-buffer-done of 72 bytes where the layout needs 100).

use alloc::vec::Vec;

use crate::hfi::{Message, cmd};

/// `VIDC_SESSION_TYPE_DEC`.
pub const DOMAIN_DECODER: u32 = 2;

/// `HFI_VIDEO_CODEC_*`.
pub mod codec {
    pub const H264: u32 = 0x2;
    pub const MPEG2: u32 = 0x10;
    pub const VP8: u32 = 0x1000;
    pub const HEVC: u32 = 0x2000;
    pub const VP9: u32 = 0x4000;
}

/// `HFI_BUFFER_*`.
pub mod buffer {
    pub const INPUT: u32 = 0x1;
    pub const OUTPUT: u32 = 0x2;
    pub const OUTPUT2: u32 = 0x3;
    pub const PERSIST: u32 = 0x4;
    pub const PERSIST_1: u32 = 0x5;
    pub const SCRATCH: u32 = 0x6;
    pub const SCRATCH_1: u32 = 0x7;
    pub const SCRATCH_2: u32 = 0x8;
    /// `intbufs_alloc`'s order for 4xx.
    pub const INTERNAL: [u32; 5] = [SCRATCH, SCRATCH_1, SCRATCH_2, PERSIST, PERSIST_1];
}

/// Session properties, `hfi_helper.h`.
pub mod property {
    pub const FRAME_SIZE: u32 = 0x1001;
    pub const UNCOMPRESSED_FORMAT_SELECT: u32 = 0x1003;
    pub const PROFILE_LEVEL_CURRENT: u32 = 0x1005;
    pub const WORK_MODE: u32 = 0x1015;
    pub const VIDEOCORES_USAGE: u32 = 0x2002;
    pub const BUFFER_COUNT_ACTUAL: u32 = 0x20_1001;
    pub const BUFFER_SIZE_ACTUAL: u32 = 0x20_100c;
    pub const BUFFER_REQUIREMENTS: u32 = 0x20_2001;
    pub const MULTI_STREAM: u32 = 0x100_3001;
    pub const CONCEAL_COLOR: u32 = 0x100_3002;
    pub const PIXEL_BITDEPTH: u32 = 0x100_3007;
    pub const PIC_STRUCT: u32 = 0x100_3009;
    pub const COLOUR_SPACE: u32 = 0x100_300a;
    pub const SUFFICIENT_SEQCHANGE_EVENT: u32 = 0x100_300b;
    pub const DPB_COUNTS: u32 = 0x120_300e;
    pub const ENTROPY: u32 = 0x120_4004;
    pub const INPUT_CROP: u32 = 0x0700_000e;
}

/// `HFI_COLOR_FORMAT_NV12` and its UBWC form.
pub const NV12: u32 = 0x2;
pub const NV12_UBWC: u32 = 0x8002;
/// `HFI_FLUSH_*`.
pub const FLUSH_OUTPUT: u32 = 0x100_0002;
pub const FLUSH_ALL: u32 = 0x100_0004;
/// `HFI_BUFFERFLAG_*`.
pub mod flag {
    pub const EOS: u32 = 0x1;
    pub const DATACORRUPT: u32 = 0x8;
    pub const TIMESTAMPINVALID: u32 = 0x100;
    pub const READONLY: u32 = 0x200;
    pub const DROP_FRAME: u32 = 0x2000_0000;
}
/// `HFI_EVENT_*`.
pub mod event {
    pub const SESSION_ERROR: u32 = 0x2;
    pub const SEQUENCE_CHANGED: u32 = 0x100_0003;
    pub const RELEASE_BUFFER_REFERENCE: u32 = 0x100_0006;
}
/// The address Linux puts in the end-of-stream input buffer: nothing is there, the firmware only reads the flag.
pub const EOS_ADDRESS: u32 = 0xdead_b000;

fn packet(kind: u32, session: u32, body: &[u32]) -> Vec<u32> {
    let mut p = Vec::with_capacity(3 + body.len());
    p.push(((3 + body.len()) * 4) as u32);
    p.push(kind);
    p.push(session);
    p.extend_from_slice(body);
    p
}

/// `pkt_session_init`.
pub fn init(session: u32, codec: u32) -> Vec<u32> {
    packet(cmd::SYS_SESSION_INIT, session, &[DOMAIN_DECODER, codec])
}

/// `pkt_session_cmd`: end, abort, load resources, start, stop, release resources, continue.
pub fn command(kind: u32, session: u32) -> Vec<u32> {
    packet(kind, session, &[])
}

/// `pkt_session_set_property_4xx`: one property and its payload.
pub fn set_property(session: u32, property: u32, payload: &[u32]) -> Vec<u32> {
    let mut body = Vec::with_capacity(2 + payload.len());
    body.push(1);
    body.push(property);
    body.extend_from_slice(payload);
    packet(cmd::SESSION_SET_PROPERTY, session, &body)
}

/// `pkt_session_get_property_4xx`.
pub fn get_property(session: u32, property: u32) -> Vec<u32> {
    packet(cmd::SESSION_GET_PROPERTY, session, &[1, property])
}

/// `pkt_session_set_buffers` for one internal buffer: no extradata, the minimum size the size itself.
pub fn set_buffer(session: u32, kind: u32, size: u32, address: u32) -> Vec<u32> {
    packet(cmd::SESSION_SET_BUFFERS, session, &[kind, size, 0, size, 1, address])
}

/// `pkt_session_unset_buffers` for one internal buffer, asking for the answer.
pub fn release_buffer(session: u32, kind: u32, size: u32, address: u32) -> Vec<u32> {
    packet(cmd::SESSION_RELEASE_BUFFERS, session, &[kind, size, 0, 1, 1, address])
}

/// One compressed input buffer: `pkt_session_etb_decoder`, with the two words Linux leaves unset written as 0. `time_us` is the
/// buffer's time stamp in microseconds.
pub struct Input {
    pub time_us: u64,
    pub flags: u32,
    pub offset: u32,
    pub alloc_len: u32,
    pub filled_len: u32,
    pub tag: u32,
    pub address: u32,
}

pub fn empty_buffer(session: u32, i: &Input) -> Vec<u32> {
    packet(
        cmd::SESSION_EMPTY_BUFFER,
        session,
        &[(i.time_us >> 32) as u32, i.time_us as u32, i.flags, 0, 0, i.offset, i.alloc_len, i.filled_len, i.tag, i.address, 0, 0],
    )
}

/// `pkt_session_ftb`: stream 0 is the DPB (`HFI_BUFFER_OUTPUT`), 1 the linear picture (`HFI_BUFFER_OUTPUT2`).
pub fn fill_buffer(session: u32, kind: u32, alloc_len: u32, tag: u32, address: u32) -> Vec<u32> {
    let stream = if kind == buffer::OUTPUT2 { 1 } else { 0 };
    packet(cmd::SESSION_FILL_BUFFER, session, &[stream, 0, alloc_len, 0, tag, address, 0, 0])
}

/// `pkt_session_flush`.
pub fn flush(session: u32, kind: u32) -> Vec<u32> {
    packet(cmd::SESSION_FLUSH, session, &[kind])
}

/// What went wrong with a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bad {
    /// Shorter than its layout.
    Short(u32),
    /// A field out of what the layout allows.
    Field(u32),
    /// A property this driver does not know in an event: its size is unknown, so nothing after it can be read.
    Property(u32),
}

/// The session id and error of a session's answer (`hfi_session_*_done`: header, session id, error).
pub fn done(m: &Message) -> Result<(u32, u32), Bad> {
    if m.words.len() < 4 {
        return Err(Bad::Short(m.kind()));
    }
    Ok((m.words[2], m.words[3]))
}

/// One entry of `struct hfi_buffer_requirements` with 4xx's names: `count_min` is the `hold_count` word, as `hfi_bufreq_get_count_min`
/// reads it there.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Requirement {
    pub kind: u32,
    pub size: u32,
    pub count_min: u32,
    pub count_actual: u32,
    pub contiguous: u32,
    pub alignment: u32,
}

/// `HFI_BUFFER_TYPE_MAX`: Linux takes at most ten entries.
pub const MAX_REQUIREMENTS: usize = 10;

/// `session_get_prop_buf_req`: the table of a `SESSION_PROPERTY_INFO` for the buffer requirements.
pub fn requirements(m: &Message) -> Result<Vec<Requirement>, Bad> {
    let w = m.words;
    if w.len() < 5 + 8 || w[3] == 0 || w[4] != property::BUFFER_REQUIREMENTS {
        return Err(Bad::Short(m.kind()));
    }
    let rest = &w[5..];
    if rest.len() % 8 != 0 || rest.len() / 8 > MAX_REQUIREMENTS {
        return Err(Bad::Field(rest.len() as u32));
    }
    Ok(rest
        .chunks(8)
        .map(|e| Requirement { kind: e[0], size: e[1], count_min: e[3], count_actual: e[5], contiguous: e[6], alignment: e[7] })
        .collect())
}

/// What the sequence-changed event says about the stream (`event_seq_changed`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Sequence {
    pub sufficient: bool,
    pub width: u32,
    pub height: u32,
    pub profile: u32,
    pub level: u32,
    /// 0 for 8 bits.
    pub bit_depth: u32,
    pub pic_struct: u32,
    pub colour_space: u32,
    pub entropy: u32,
    /// left, top, width, height; all 0 when the event did not say.
    pub crop: [u32; 4],
    pub dpb_count: u32,
    /// The DPB counts' `fw_min_cnt` or the requirements' `count_min`, whichever came.
    pub buffer_count: u32,
}

/// `HFI_EVENT_DATA_SEQUENCE_CHANGED_SUFFICIENT_BUF_RESOURCES` and its insufficient twin.
const SUFFICIENT: u32 = 0x100_0001;
const INSUFFICIENT: u32 = 0x100_0002;

/// An `EVENT_NOTIFY`'s session, event and two data words.
pub fn event(m: &Message) -> Result<(u32, u32, u32, u32), Bad> {
    let w = m.words;
    if w.len() < 6 {
        return Err(Bad::Short(m.kind()));
    }
    Ok((w[2], w[3], w[4], w[5]))
}

/// The properties after a sequence-changed event's header, each read only as far as its layout goes. A property this driver does not
/// know is passed over only when it is the last.
pub fn sequence(m: &Message) -> Result<Sequence, Bad> {
    let (_, id, data1, count) = event(m)?;
    if id != event::SEQUENCE_CHANGED {
        return Err(Bad::Field(id));
    }
    let mut s = Sequence {
        sufficient: match data1 {
            SUFFICIENT => true,
            INSUFFICIENT => false,
            other => return Err(Bad::Field(other)),
        },
        ..Sequence::default()
    };
    if count == 0 {
        return Err(Bad::Field(0));
    }
    let w = m.words;
    let mut at = 6;
    for i in 0..count {
        let ptype = *w.get(at).ok_or(Bad::Short(m.kind()))?;
        at += 1;
        let words = match ptype {
            property::FRAME_SIZE => 3,
            property::PROFILE_LEVEL_CURRENT => 2,
            property::PIXEL_BITDEPTH => 2,
            property::PIC_STRUCT | property::COLOUR_SPACE | property::ENTROPY => 1,
            property::BUFFER_REQUIREMENTS => 8,
            property::INPUT_CROP => 7,
            property::DPB_COUNTS => 5,
            // Linux reads a property it does not know as having no payload and goes on. Firmware 5.4.32 ends its event with
            // 0x100300b and four words (seen on the laptop); last, it hides nothing, so it is passed over. Anywhere else its size
            // would decide where the next property starts, and that is refused.
            _ if i + 1 == count => break,
            other => return Err(Bad::Property(other)),
        };
        let p = w.get(at..at + words).ok_or(Bad::Short(m.kind()))?;
        at += words;
        match ptype {
            property::FRAME_SIZE => (s.width, s.height) = (p[1], p[2]),
            property::PROFILE_LEVEL_CURRENT => (s.profile, s.level) = (p[0], p[1]),
            property::PIXEL_BITDEPTH => s.bit_depth = p[1],
            property::PIC_STRUCT => s.pic_struct = p[0],
            property::COLOUR_SPACE => s.colour_space = p[0],
            property::ENTROPY => s.entropy = p[0],
            property::BUFFER_REQUIREMENTS => s.buffer_count = p[3],
            property::INPUT_CROP => s.crop = [p[3], p[4], p[5], p[6]],
            _ => {
                s.dpb_count = p[0];
                s.buffer_count = p[4];
            }
        }
    }
    Ok(s)
}

/// An empty-buffer-done (`hfi_msg_session_empty_buffer_done_pkt`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InputDone {
    pub error: u32,
    pub offset: u32,
    pub filled_len: u32,
    pub tag: u32,
    pub address: u32,
}

pub fn input_done(m: &Message) -> Result<InputDone, Bad> {
    let w = m.words;
    if w.len() < 9 {
        return Err(Bad::Short(m.kind()));
    }
    Ok(InputDone { error: w[3], offset: w[4], filled_len: w[5], tag: w[6], address: w[7] })
}

/// A fill-buffer-done of a decoder (`hfi_msg_session_fbd_uncompressed_plane0_pkt`, 100 bytes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutputDone {
    /// 0: a DPB buffer came back; 1: a picture in a linear buffer.
    pub stream: u32,
    pub error: u32,
    pub time_us: u64,
    pub flags: u32,
    pub alloc_len: u32,
    pub filled_len: u32,
    pub offset: u32,
    pub width: u32,
    pub height: u32,
    pub tag: u32,
    pub picture_type: u32,
    pub address: u32,
}

pub fn output_done(m: &Message) -> Result<OutputDone, Bad> {
    let w = m.words;
    if w.len() < 25 {
        return Err(Bad::Short(m.kind()));
    }
    if w[3] > 1 {
        return Err(Bad::Field(w[3]));
    }
    Ok(OutputDone {
        stream: w[3],
        error: w[5],
        time_us: (u64::from(w[6]) << 32) | u64::from(w[7]),
        flags: w[8],
        alloc_len: w[12],
        filled_len: w[13],
        offset: w[14],
        width: w[15],
        height: w[16],
        tag: w[21],
        picture_type: w[22],
        address: w[23],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sequence-changed event the laptop's firmware (VIDEO.VE.5.4-00032) sent for film-1080.mp4, 2026-11-04.
    const LAPTOP_EVENT: [u32; 44] = [
        0xb0, 0x21001, 1, 0x1000003, 0x1000002, 9, 0x1001, 2, 0x780, 0x320, 0x1005, 4, 0x1000, 0x1003007, 2, 0, 0x1003009, 1,
        0x100300a, 0, 0x1204004, 2, 0x202001, 0, 0, 0, 6, 0, 0, 0, 0, 0x700000e, 0x1c, 0, 0, 0, 0, 0x780, 0x320, 0x100300b, 4, 4, 4,
        2,
    ];

    #[test]
    fn the_laptops_sequence_event() {
        let s = sequence(&Message { words: &LAPTOP_EVENT }).unwrap();
        assert!(!s.sufficient);
        assert_eq!((s.width, s.height, s.bit_depth, s.pic_struct, s.entropy, s.buffer_count), (1920, 800, 0, 1, 2, 6));
        assert_eq!(s.crop, [0, 0, 1920, 800]);
        assert_eq!((s.profile, s.level), (4, 0x1000));
    }

    #[test]
    fn an_unknown_property_in_the_middle_is_refused() {
        let mut e = LAPTOP_EVENT.to_vec();
        e[5] = 10;
        e.extend_from_slice(&[0x1001, 2, 16, 16]);
        e[0] = (e.len() * 4) as u32;
        assert_eq!(sequence(&Message { words: &e }), Err(Bad::Property(0x100300b)));
    }
}
