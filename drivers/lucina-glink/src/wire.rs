// Derived from Linux drivers/rpmsg/qcom_glink_native.c and qcom_glink_native.h (v7.1): command numbers, the
// message layouts and the alignment rules. Those files are GPL-2.0, Copyright (c) 2016-2017 Linaro Ltd. This
// file is a re-implementation of the wire format in Rust, not a copy of the C; it stays under the same licence.

//! The native GLINK wire format: command numbers, header layout and frame encoders.
//!
//! Every message starts with an 8-byte header, `struct glink_msg_hdr` (`qcom_glink_native.c:35`): `cmd: u16`,
//! `param1: u16`, `param2: u32`, little endian. Frames sit in the FIFO back to back, each starting on an 8-byte
//! boundary; the writer pads with unwritten bytes (here: zeros) up to the next multiple of 8.
//!
//! What the two parameters mean depends on the command, and the id they carry is the one the *receiver* looks up:
//!
//! | command | param1 | param2 | trailing bytes |
//! |---|---|---|---|
//! | `VERSION`, `VERSION_ACK` | version | features | none |
//! | `OPEN` | sender's lcid | name length incl. NUL (low 16 bits; the high 16 are "prio") | the name, padded to 8 |
//! | `OPEN_ACK` | the id from the `OPEN` being acked (= the receiver's lcid) | 0 | none |
//! | `CLOSE` | sender's lcid | 0 | none |
//! | `CLOSE_ACK` | the id from the `CLOSE` being acked (= the receiver's lcid) | 0 | none |
//! | `INTENT` | sender's lcid | count | `count` pairs of (size: u32, intent id: u32) |
//! | `RX_DONE`, `RX_DONE_W_REUSE` | sender's lcid | the receiver's intent id | none |
//! | `RX_INTENT_REQ` | sender's lcid | requested size | none |
//! | `RX_INTENT_REQ_ACK` | sender's lcid | 1 granted, 0 denied | none |
//! | `TX_DATA`, `TX_DATA_CONT` | sender's lcid | the intent id of the receiver | chunk_size: u32, left_size: u32, then the payload |
//! | `SIGNALS` | sender's lcid | signal bits | none |
//! | `READ_NOTIF` | 0 | 0 | none |
//!
//! "Sender's lcid" is the receiver's rcid, which is why the receiver looks it up in its remote-id table. The two
//! acks are the odd ones out: they echo the id the receiver itself chose.

use alloc::vec::Vec;

/// `GLINK_NAME_SIZE` (`qcom_glink_native.c:29`): names, including the NUL, are at most this long.
pub const NAME_SIZE: usize = 32;
/// `GLINK_VERSION_1` (`qcom_glink_native.c:30`).
pub const VERSION_1: u16 = 1;
/// `RPM_GLINK_CID_MIN` (`qcom_glink_native.c:32`): the smallest local channel id.
pub const CID_MIN: u32 = 1;
/// `RPM_GLINK_CID_MAX` (`qcom_glink_native.c:33`): the exclusive upper bound of local channel ids.
pub const CID_MAX: u32 = 65536;

pub const CMD_VERSION: u16 = 0;
pub const CMD_VERSION_ACK: u16 = 1;
pub const CMD_OPEN: u16 = 2;
pub const CMD_CLOSE: u16 = 3;
pub const CMD_OPEN_ACK: u16 = 4;
pub const CMD_INTENT: u16 = 5;
pub const CMD_RX_DONE: u16 = 6;
pub const CMD_RX_INTENT_REQ: u16 = 7;
pub const CMD_RX_INTENT_REQ_ACK: u16 = 8;
pub const CMD_TX_DATA: u16 = 9;
// 10 is not used by the driver.
pub const CMD_CLOSE_ACK: u16 = 11;
pub const CMD_TX_DATA_CONT: u16 = 12;
pub const CMD_READ_NOTIF: u16 = 13;
pub const CMD_RX_DONE_W_REUSE: u16 = 14;
pub const CMD_SIGNALS: u16 = 15;

/// `GLINK_FEATURE_INTENT_REUSE` (`qcom_glink_native.h:11`): an intent may be used for more than one message.
pub const FEATURE_INTENT_REUSE: u32 = 1 << 0;
/// `GLINK_FEATURE_MIGRATION` (`qcom_glink_native.h:12`).
pub const FEATURE_MIGRATION: u32 = 1 << 1;
/// `GLINK_FEATURE_TRACER_PKT` (`qcom_glink_native.h:13`).
pub const FEATURE_TRACER_PKT: u32 = 1 << 2;

/// `NATIVE_DSR_SIG` and `NATIVE_DTR_SIG` (`qcom_glink_native.c:218-219`): the same bit.
pub const SIG_DSR: u32 = 1 << 31;
/// `NATIVE_CTS_SIG` and `NATIVE_RTS_SIG` (`qcom_glink_native.c:220-221`): the same bit.
pub const SIG_CTS: u32 = 1 << 30;

/// Size of `struct glink_msg_hdr`.
pub const HDR_LEN: usize = 8;
/// Size of the `TX_DATA` header: `struct glink_msg_hdr` plus `chunk_size` and `left_size`.
pub const DATA_HDR_LEN: usize = 16;

/// `ALIGN(n, 8)`.
pub const fn align8(n: usize) -> usize {
    (n + 7) & !7
}

/// A decoded message header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub cmd: u16,
    pub param1: u16,
    pub param2: u32,
}

impl Header {
    pub fn decode(b: &[u8; HDR_LEN]) -> Header {
        Header {
            cmd: u16::from_le_bytes([b[0], b[1]]),
            param1: u16::from_le_bytes([b[2], b[3]]),
            param2: u32::from_le_bytes([b[4], b[5], b[6], b[7]]),
        }
    }

    pub fn encode(&self) -> [u8; HDR_LEN] {
        let mut b = [0u8; HDR_LEN];
        b[0..2].copy_from_slice(&self.cmd.to_le_bytes());
        b[2..4].copy_from_slice(&self.param1.to_le_bytes());
        b[4..8].copy_from_slice(&self.param2.to_le_bytes());
        b
    }
}

/// A frame that is only a header: everything except `OPEN`, `INTENT` and the data commands.
pub fn simple(cmd: u16, param1: u16, param2: u32) -> Vec<u8> {
    Header {
        cmd,
        param1,
        param2,
    }
    .encode()
    .to_vec()
}

/// `qcom_glink_send_open_req` (`qcom_glink_native.c:481-`): `param2` is the name length including the NUL, the
/// name follows, and the whole frame is padded to 8 bytes. The caller has checked the name length.
pub fn open(lcid: u16, name: &str) -> Vec<u8> {
    let name_len = name.len() + 1;
    let mut f = Vec::with_capacity(align8(HDR_LEN + name_len));
    f.extend_from_slice(
        &Header {
            cmd: CMD_OPEN,
            param1: lcid,
            param2: name_len as u32,
        }
        .encode(),
    );
    f.extend_from_slice(name.as_bytes());
    f.push(0);
    f.resize(align8(HDR_LEN + name_len), 0);
    f
}

/// `qcom_glink_advertise_intent` (`qcom_glink_native.c:723-`): always one intent per message, 16 bytes.
pub fn intent(lcid: u16, size: u32, iid: u32) -> Vec<u8> {
    let mut f = Vec::with_capacity(16);
    f.extend_from_slice(
        &Header {
            cmd: CMD_INTENT,
            param1: lcid,
            param2: 1,
        }
        .encode(),
    );
    f.extend_from_slice(&size.to_le_bytes());
    f.extend_from_slice(&iid.to_le_bytes());
    f
}

/// The 16-byte header of `TX_DATA` and `TX_DATA_CONT` (`__qcom_glink_send`, `qcom_glink_native.c:1530-`).
pub fn data_header(cmd: u16, lcid: u16, iid: u32, chunk: u32, left: u32) -> [u8; DATA_HDR_LEN] {
    let mut b = [0u8; DATA_HDR_LEN];
    b[0..8].copy_from_slice(
        &Header {
            cmd,
            param1: lcid,
            param2: iid,
        }
        .encode(),
    );
    b[8..12].copy_from_slice(&chunk.to_le_bytes());
    b[12..16].copy_from_slice(&left.to_le_bytes());
    b
}
