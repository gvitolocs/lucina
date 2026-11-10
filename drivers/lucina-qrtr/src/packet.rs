// SPDX-License-Identifier: GPL-2.0
//
// Derived from Linux net/qrtr/af_qrtr.c (GPL-2.0-only, Copyright (c) 2015 Sony Mobile Communications Inc.,
// Copyright (c) 2013 The Linux Foundation) and include/uapi/linux/qrtr.h (GPL-2.0 WITH Linux-syscall-note):
// the packet headers, the receive-side checks of `qrtr_endpoint_post()`, the transmit-side header and padding
// of `qrtr_node_enqueue()`, and `struct qrtr_ctrl_pkt`.

//! The QRTR wire format: packet headers, the checks an incoming packet must pass, and control packets.
//!
//! A packet is one message of the IPCRTR channel (one GLINK or SMD message). It is a header, the payload
//! (`size` bytes), and zero padding up to a multiple of four bytes; the whole message is a multiple of four.
//!
//! * [`HDR_V1_LEN`]: `struct qrtr_hdr_v1`, eight little-endian 32-bit words, `version` 1. All Linux ever sends.
//! * [`HDR_V2_LEN`]: `struct qrtr_hdr_v2`, 16 bytes with 16-bit node and port ids and `version` 3, followed by
//!   `optlen` bytes of options that are skipped. Linux receives these but never sends them.
//! * [`decode`] is the receive side of `qrtr_endpoint_post()` up to the point where the packet is queued:
//!   every `goto err` there is a [`PacketError`] here.
//! * [`encode_v1`] is the transmit side of `qrtr_node_enqueue()`: push the header, pad to four.
//! * [`CtrlPkt`] is `struct qrtr_ctrl_pkt`: the 20-byte payload of every control packet.

use alloc::vec::Vec;

/// `QRTR_PROTO_VER_1`.
pub const PROTO_VER_1: u8 = 1;
/// `QRTR_PROTO_VER_2`: the second header layout is protocol version 3.
pub const PROTO_VER_2: u8 = 3;

/// `sizeof(struct qrtr_hdr_v1)`.
pub const HDR_V1_LEN: usize = 32;
/// `sizeof(struct qrtr_hdr_v2)`, without the options.
pub const HDR_V2_LEN: usize = 16;
/// `QRTR_FLAGS_CONFIRM_RX`.
pub const FLAGS_CONFIRM_RX: u8 = 1;

/// `QRTR_NODE_BCAST`: the destination node of a broadcast.
pub const NODE_BCAST: u32 = 0xffff_ffff;
/// `QRTR_PORT_CTRL`: the name service's port.
pub const PORT_CTRL: u32 = 0xffff_fffe;
/// `QRTR_PORT_CTRL_LEGACY`: what old peers put in a destination port to mean [`PORT_CTRL`].
pub const PORT_CTRL_LEGACY: u32 = 0xffff;
/// `QRTR_EP_NID_AUTO`: an endpoint whose node id is not known yet.
pub const EP_NID_AUTO: u32 = 0xffff_ffff;
/// `QRTR_MIN_EPH_SOCKET` and `QRTR_MAX_EPH_SOCKET`: where automatic port numbers come from.
pub const MIN_EPH_PORT: u32 = 0x4000;
pub const MAX_EPH_PORT: u32 = 0x7fff;

/// `QRTR_TX_FLOW_HIGH` and `QRTR_TX_FLOW_LOW`.
pub const TX_FLOW_HIGH: u32 = 10;
pub const TX_FLOW_LOW: u32 = 5;

/// The longest payload `qrtr_sendmsg()` takes (`len > 65535` is `-EMSGSIZE`).
pub const MAX_PAYLOAD: usize = 65535;

/// `enum qrtr_pkt_type`. The packet type in the header; for control packets the same number is the `cmd` in
/// the payload.
pub mod pkt_type {
    pub const DATA: u32 = 1;
    pub const HELLO: u32 = 2;
    pub const BYE: u32 = 3;
    pub const NEW_SERVER: u32 = 4;
    pub const DEL_SERVER: u32 = 5;
    pub const DEL_CLIENT: u32 = 6;
    pub const RESUME_TX: u32 = 7;
    pub const EXIT: u32 = 8;
    pub const PING: u32 = 9;
    pub const NEW_LOOKUP: u32 = 10;
    pub const DEL_LOOKUP: u32 = 11;
}

/// `sizeof(struct qrtr_ctrl_pkt)`: the command and four words, whether or not a command uses them all.
pub const CTRL_PKT_LEN: usize = 20;

/// Why [`decode`] refused a packet. All of them are `-EINVAL` in the C, and the transport drops the message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacketError {
    /// Empty, or not a multiple of four bytes.
    BadLength,
    /// The first byte is neither 1 nor 3.
    BadVersion(u8),
    /// Shorter than its own header (and options).
    ShortHeader,
    /// `size` is zero, or the message is not exactly header, options, `size` bytes and padding.
    BadSize,
    /// A NEW_SERVER or RESUME_TX with a payload shorter than a control packet.
    ShortControl,
    /// A packet that is neither data nor resume-tx, for a port other than the name service's.
    NotForNameService,
}

/// A received packet, header fields widened to 32 bits, payload without padding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Packet<'a> {
    pub version: u8,
    /// The type as the C stores it, in a `u8`: a v1 type word of 0x102 is HELLO.
    pub ptype: u32,
    pub src_node: u32,
    pub src_port: u32,
    pub dst_node: u32,
    /// With [`PORT_CTRL_LEGACY`] already turned into [`PORT_CTRL`], and in v2 the 16-bit form of
    /// [`PORT_CTRL`] too.
    pub dst_port: u32,
    pub confirm_rx: bool,
    pub payload: &'a [u8],
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn le16(b: &[u8], at: usize) -> u32 {
    u32::from(u16::from_le_bytes([b[at], b[at + 1]]))
}

/// Round up to a multiple of four, in 64 bits so that no wire value wraps.
fn align4(n: u64) -> u64 {
    (n + 3) & !3
}

/// The receive checks of `qrtr_endpoint_post()`.
pub fn decode(data: &[u8]) -> Result<Packet<'_>, PacketError> {
    if data.is_empty() || data.len() & 3 != 0 {
        return Err(PacketError::BadLength);
    }
    // "Version field in v1 is little endian, so this works for both cases"
    let version = data[0];
    let (hdrlen, ptype, src_node, mut src_port, dst_node, mut dst_port, confirm_rx, size) =
        match version {
            PROTO_VER_1 => {
                if data.len() < HDR_V1_LEN {
                    return Err(PacketError::ShortHeader);
                }
                (
                    HDR_V1_LEN,
                    // cb->type is a u8 in the C.
                    u32::from(le32(data, 4) as u8),
                    le32(data, 8),
                    le32(data, 12),
                    le32(data, 24),
                    le32(data, 28),
                    le32(data, 16) != 0,
                    le32(data, 20),
                )
            }
            PROTO_VER_2 => {
                if data.len() < HDR_V2_LEN {
                    return Err(PacketError::ShortHeader);
                }
                (
                    HDR_V2_LEN + usize::from(data[3]),
                    u32::from(data[1]),
                    le16(data, 8),
                    le16(data, 10),
                    le16(data, 12),
                    le16(data, 14),
                    data[2] & FLAGS_CONFIRM_RX != 0,
                    le32(data, 4),
                )
            }
            v => return Err(PacketError::BadVersion(v)),
        };

    // In v2 the control port is 16 bits wide.
    if version == PROTO_VER_2 {
        if src_port == u32::from(PORT_CTRL as u16) {
            src_port = PORT_CTRL;
        }
        if dst_port == u32::from(PORT_CTRL as u16) {
            dst_port = PORT_CTRL;
        }
    }

    if dst_port == PORT_CTRL_LEGACY {
        dst_port = PORT_CTRL;
    }

    let size64 = u64::from(size);
    if size == 0
        || size64 > data.len() as u64
        || data.len() as u64 != align4(size64) + hdrlen as u64
    {
        return Err(PacketError::BadSize);
    }
    let size = size as usize;
    if (ptype == pkt_type::NEW_SERVER || ptype == pkt_type::RESUME_TX) && size < CTRL_PKT_LEN {
        return Err(PacketError::ShortControl);
    }
    if dst_port != PORT_CTRL && ptype != pkt_type::DATA && ptype != pkt_type::RESUME_TX {
        return Err(PacketError::NotForNameService);
    }

    Ok(Packet {
        version,
        ptype,
        src_node,
        src_port,
        dst_node,
        dst_port,
        confirm_rx,
        payload: &data[hdrlen..hdrlen + size],
    })
}

/// A v1 packet as `qrtr_node_enqueue()` builds it: header, payload, zero padding to a multiple of four.
pub fn encode_v1(
    ptype: u32,
    src: (u32, u32),
    dst: (u32, u32),
    confirm_rx: bool,
    payload: &[u8],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(HDR_V1_LEN + payload.len() + 3);
    out.extend_from_slice(&u32::from(PROTO_VER_1).to_le_bytes());
    out.extend_from_slice(&ptype.to_le_bytes());
    out.extend_from_slice(&src.0.to_le_bytes());
    out.extend_from_slice(&src.1.to_le_bytes());
    out.extend_from_slice(&u32::from(confirm_rx).to_le_bytes());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&dst.0.to_le_bytes());
    out.extend_from_slice(&dst.1.to_le_bytes());
    out.extend_from_slice(payload);
    out.resize(HDR_V1_LEN + align4(payload.len() as u64) as usize, 0);
    out
}

/// A v2 packet, which Linux never sends but peers may. Node and port ids are cut to 16 bits, and
/// [`PORT_CTRL`] becomes its 16-bit form. `options` go between the header and the payload.
pub fn encode_v2(
    ptype: u8,
    confirm_rx: bool,
    src: (u32, u32),
    dst: (u32, u32),
    options: &[u8],
    payload: &[u8],
) -> Option<Vec<u8>> {
    let optlen = u8::try_from(options.len()).ok()?;
    let mut out = Vec::with_capacity(HDR_V2_LEN + options.len() + payload.len() + 3);
    out.push(PROTO_VER_2);
    out.push(ptype);
    out.push(if confirm_rx { FLAGS_CONFIRM_RX } else { 0 });
    out.push(optlen);
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    for v in [src.0, src.1, dst.0, dst.1] {
        out.extend_from_slice(&(v as u16).to_le_bytes());
    }
    out.extend_from_slice(options);
    out.extend_from_slice(payload);
    let total = HDR_V2_LEN + options.len() + align4(payload.len() as u64) as usize;
    out.resize(total, 0);
    Some(out)
}

/// An observed service, as in `struct qrtr_ctrl_pkt.server`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Server {
    pub service: u32,
    pub instance: u32,
    pub node: u32,
    pub port: u32,
}

/// `struct qrtr_ctrl_pkt`: `cmd` and then a union of `server { service, instance, node, port }` and
/// `client { node, port }`. The client fields lie over the first two server words, so `RESUME_TX`, `BYE`
/// and `DEL_CLIENT` carry their node and port where a server record has its service and instance.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CtrlPkt {
    pub cmd: u32,
    words: [u32; 4],
}

impl CtrlPkt {
    /// A command with an all-zero union (`HELLO`, `PING`, `EXIT`, an empty lookup answer).
    pub fn new(cmd: u32) -> CtrlPkt {
        CtrlPkt { cmd, words: [0; 4] }
    }

    /// `NEW_SERVER`, `DEL_SERVER`, or `NEW_LOOKUP` and `DEL_LOOKUP` (which fill only service and instance).
    pub fn with_server(cmd: u32, s: Server) -> CtrlPkt {
        CtrlPkt {
            cmd,
            words: [s.service, s.instance, s.node, s.port],
        }
    }

    /// `BYE`, `DEL_CLIENT` and `RESUME_TX`.
    pub fn with_client(cmd: u32, node: u32, port: u32) -> CtrlPkt {
        CtrlPkt {
            cmd,
            words: [node, port, 0, 0],
        }
    }

    pub fn server(&self) -> Server {
        Server {
            service: self.words[0],
            instance: self.words[1],
            node: self.words[2],
            port: self.words[3],
        }
    }

    /// `(client.node, client.port)`.
    pub fn client(&self) -> (u32, u32) {
        (self.words[0], self.words[1])
    }

    pub fn to_bytes(&self) -> [u8; CTRL_PKT_LEN] {
        let mut b = [0u8; CTRL_PKT_LEN];
        b[..4].copy_from_slice(&self.cmd.to_le_bytes());
        for (i, w) in self.words.iter().enumerate() {
            b[4 + 4 * i..8 + 4 * i].copy_from_slice(&w.to_le_bytes());
        }
        b
    }

    /// Read a payload as a control packet. The name service in Linux reads the fixed-size struct out of a
    /// receive buffer whatever the payload length was, so words past the end of a short payload are stale
    /// bytes there; here they are zero. A payload under four bytes has no command: `None`.
    pub fn parse(payload: &[u8]) -> Option<CtrlPkt> {
        if payload.len() < 4 {
            return None;
        }
        let mut b = [0u8; CTRL_PKT_LEN];
        let n = payload.len().min(CTRL_PKT_LEN);
        b[..n].copy_from_slice(&payload[..n]);
        Some(CtrlPkt {
            cmd: le32(&b, 0),
            words: [le32(&b, 4), le32(&b, 8), le32(&b, 12), le32(&b, 16)],
        })
    }
}
