//! The APR header (`include/linux/soc/qcom/apr.h`) and the services on the ADSP's `apr_audio_svc` channel.

use alloc::vec::Vec;

pub const HDR_SIZE: usize = 20;
/// `APR_SEQ_CMD_HDR_FIELD`: message type 2, header of 5 words, version 0.
pub const SEQ_CMD_HDR_FIELD: u16 = 0x250;
/// `APR_BASIC_RSP_RESULT`: the payload is the command's opcode and a status.
pub const BASIC_RSP_RESULT: u32 = 0x0001_10E8;
pub const DOMAIN_ADSP: u8 = 4;
pub const DOMAIN_APPS: u8 = 5;
pub const SVC_CORE: u8 = 3;
pub const SVC_AFE: u8 = 4;
pub const SVC_ASM: u8 = 7;
pub const SVC_ADM: u8 = 8;
/// `APR_MSG_TYPE_MAX`, `APR_DOMAIN_MAX`, `APR_SVC_MAX`.
const MSG_TYPE_MAX: u16 = 4;
const DOMAIN_MAX: u8 = 6;
const SVC_MAX: u8 = 0x17;
/// `APR_PKT_VER`.
const PKT_VER: u16 = 0;

/// A packet as received. Decoding checks lengths only, and never panics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Packet<'a> {
    pub src_svc: u8,
    pub src_port: u16,
    pub dest_port: u16,
    pub token: u32,
    pub opcode: u32,
    pub payload: &'a [u8],
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// `apr_callback` and `apr_do_rx_callback`: what they drop, this drops. The header's own length may be longer than ours: the DSP can add
/// optional headers, which are skipped.
pub fn parse(buf: &[u8]) -> Option<Packet<'_>> {
    if buf.len() <= HDR_SIZE {
        return None;
    }
    let field = u16_at(buf, 0)?;
    if field & 0xf > PKT_VER + 1 || (field & 0x300) >> 8 >= MSG_TYPE_MAX {
        return None;
    }
    let hdr_len = usize::from((field & 0xf0) >> 4) * 4;
    let pkt_size = usize::from(u16_at(buf, 2)?);
    if hdr_len < HDR_SIZE || pkt_size < hdr_len || pkt_size != buf.len() {
        return None;
    }
    let (src_svc, src_domain, dest_svc, dest_domain) =
        (*buf.get(4)?, *buf.get(5)?, *buf.get(8)?, *buf.get(9)?);
    if src_domain >= DOMAIN_MAX
        || dest_domain >= DOMAIN_MAX
        || src_svc >= SVC_MAX
        || dest_svc >= SVC_MAX
    {
        return None;
    }
    Some(Packet {
        src_svc,
        src_port: u16_at(buf, 6)?,
        dest_port: u16_at(buf, 10)?,
        token: u32_at(buf, 12)?,
        opcode: u32_at(buf, 16)?,
        payload: &buf[hdr_len..pkt_size],
    })
}

impl Packet<'_> {
    /// The two words of `APR_BASIC_RSP_RESULT`, and of what else starts that way.
    pub fn result(&self) -> Option<(u32, u32)> {
        Some((u32_at(self.payload, 0)?, u32_at(self.payload, 4)?))
    }
}

/// `apr_send_pkt`: the header filled in for service `svc`.
pub fn build(
    svc: u8,
    src_port: u16,
    dest_port: u16,
    token: u32,
    opcode: u32,
    payload: &[u8],
) -> Vec<u8> {
    let size = HDR_SIZE + payload.len();
    let mut out = Vec::with_capacity(size);
    out.extend_from_slice(&SEQ_CMD_HDR_FIELD.to_le_bytes());
    out.extend_from_slice(&(size as u16).to_le_bytes());
    out.push(svc);
    out.push(DOMAIN_APPS);
    out.extend_from_slice(&src_port.to_le_bytes());
    out.push(svc);
    out.push(DOMAIN_ADSP);
    out.extend_from_slice(&dest_port.to_le_bytes());
    out.extend_from_slice(&token.to_le_bytes());
    out.extend_from_slice(&opcode.to_le_bytes());
    out.extend_from_slice(payload);
    out
}
