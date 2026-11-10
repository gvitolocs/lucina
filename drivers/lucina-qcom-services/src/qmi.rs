// SPDX-License-Identifier: BSD-3-Clause
//
// From userspace/crates/lucina-qcom/src/qmi.rs, after linux-msm qrtr's lib/qmi.c (Copyright (c) 2015, Sony Mobile
// Communications Inc.).

//! QMI messages as libqrtr encodes them: a 7-byte header (type, transaction id, message id, payload length;
//! little-endian) and then TLVs: a type byte, a 16-bit length, the value. Mandatory TLVs have types below 0x10.
//! A string in a top-level TLV has no length prefix; inside a struct it gets a one-byte one; a variable-length
//! array starts with its element count.
//!
//! This is the userspace crate's typed builder rather than `lucina-qmi`'s codec: its bytes are the ones the C
//! fixtures check, and it has what these servers need and no client side. Decoding never panics. A value too long
//! for a TLV is cut at 65535 bytes (the callers here never build one).

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

pub const REQUEST: u8 = 0;
pub const RESPONSE: u8 = 2;
pub const INDICATION: u8 = 4;
pub const HEADER_LEN: usize = 7;
pub const OPTIONAL_TLV_START: u8 = 0x10;
/// Every response's first TLV: `struct qmi_response_type_v01`.
pub const RESULT_TLV: u8 = 2;
pub const RESULT_SUCCESS: u16 = 0;
pub const RESULT_FAILURE: u16 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub kind: u8,
    pub txn: u16,
    pub msg_id: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Shorter than the header.
    Short,
    /// The header's length is not the payload's (`qmi_decode_header`).
    Length,
    /// A TLV runs past the end.
    Truncated,
    /// A mandatory TLV is missing.
    Missing(u8),
    /// A TLV's length does not fit its type.
    BadValue(u8),
    /// A mandatory TLV this message does not have: libqrtr refuses those and skips unknown optional ones.
    Unknown(u8),
}

/// A received message: its header and TLVs, borrowed from the packet.
#[derive(Debug)]
pub struct Message<'a> {
    pub header: Header,
    tlvs: Vec<(u8, &'a [u8])>,
}

/// Split a packet into header and TLVs.
pub fn parse(buf: &[u8]) -> Result<Message<'_>, Error> {
    if buf.len() < HEADER_LEN {
        return Err(Error::Short);
    }
    let header = Header {
        kind: buf[0],
        txn: u16::from_le_bytes([buf[1], buf[2]]),
        msg_id: u16::from_le_bytes([buf[3], buf[4]]),
    };
    if usize::from(u16::from_le_bytes([buf[5], buf[6]])) != buf.len() - HEADER_LEN {
        return Err(Error::Length);
    }
    let mut tlvs = Vec::new();
    let mut p = &buf[HEADER_LEN..];
    while !p.is_empty() {
        if p.len() < 3 {
            return Err(Error::Truncated);
        }
        let t = p[0];
        let len = usize::from(u16::from_le_bytes([p[1], p[2]]));
        if p.len() < 3 + len {
            return Err(Error::Truncated);
        }
        tlvs.push((t, &p[3..3 + len]));
        p = &p[3 + len..];
    }
    Ok(Message { header, tlvs })
}

impl<'a> Message<'a> {
    pub fn get(&self, t: u8) -> Option<&'a [u8]> {
        self.tlvs.iter().find(|(k, _)| *k == t).map(|(_, v)| *v)
    }

    /// Refuse a mandatory TLV that is not in `known`.
    pub fn check_known(&self, known: &[u8]) -> Result<(), Error> {
        match self
            .tlvs
            .iter()
            .find(|(t, _)| *t < OPTIONAL_TLV_START && !known.contains(t))
        {
            Some((t, _)) => Err(Error::Unknown(*t)),
            None => Ok(()),
        }
    }

    fn fixed<const N: usize>(&self, t: u8) -> Result<Option<[u8; N]>, Error> {
        match self.get(t) {
            None => Ok(None),
            Some(v) => v.try_into().map(Some).map_err(|_| Error::BadValue(t)),
        }
    }

    pub fn u8(&self, t: u8) -> Result<Option<u8>, Error> {
        Ok(self.fixed::<1>(t)?.map(|b| b[0]))
    }

    pub fn u16(&self, t: u8) -> Result<Option<u16>, Error> {
        Ok(self.fixed(t)?.map(u16::from_le_bytes))
    }

    pub fn u32(&self, t: u8) -> Result<Option<u32>, Error> {
        Ok(self.fixed(t)?.map(u32::from_le_bytes))
    }

    pub fn u64(&self, t: u8) -> Result<Option<u64>, Error> {
        Ok(self.fixed(t)?.map(u64::from_le_bytes))
    }

    pub fn need_u8(&self, t: u8) -> Result<u8, Error> {
        self.u8(t)?.ok_or(Error::Missing(t))
    }

    pub fn need_u32(&self, t: u8) -> Result<u32, Error> {
        self.u32(t)?.ok_or(Error::Missing(t))
    }

    /// A top-level string TLV of at most `max` bytes.
    pub fn string(&self, t: u8, max: usize) -> Result<Option<String>, Error> {
        match self.get(t) {
            None => Ok(None),
            Some(v) if v.len() > max => Err(Error::BadValue(t)),
            Some(v) => Ok(Some(String::from_utf8_lossy(v).into_owned())),
        }
    }

    pub fn need_string(&self, t: u8, max: usize) -> Result<String, Error> {
        self.string(t, max)?.ok_or(Error::Missing(t))
    }

    /// The response result TLV: (result, error).
    pub fn result(&self) -> Result<(u16, u16), Error> {
        let v = self.get(RESULT_TLV).ok_or(Error::Missing(RESULT_TLV))?;
        if v.len() != 4 {
            return Err(Error::BadValue(RESULT_TLV));
        }
        Ok((
            u16::from_le_bytes([v[0], v[1]]),
            u16::from_le_bytes([v[2], v[3]]),
        ))
    }
}

/// Builds one message; TLVs go out in the order they are added (libqrtr follows its element table, so callers add
/// them in the table's order).
#[derive(Debug, Clone)]
pub struct Builder {
    buf: Vec<u8>,
}

impl Default for Builder {
    fn default() -> Self {
        Builder::new()
    }
}

impl Builder {
    pub fn new() -> Builder {
        Builder {
            buf: vec![0; HEADER_LEN],
        }
    }

    pub fn tlv(&mut self, t: u8, value: &[u8]) -> &mut Builder {
        let value = &value[..value.len().min(usize::from(u16::MAX))];
        self.buf.push(t);
        self.buf
            .extend_from_slice(&(value.len() as u16).to_le_bytes());
        self.buf.extend_from_slice(value);
        self
    }

    pub fn u8(&mut self, t: u8, v: u8) -> &mut Builder {
        self.tlv(t, &[v])
    }

    pub fn u16(&mut self, t: u8, v: u16) -> &mut Builder {
        self.tlv(t, &v.to_le_bytes())
    }

    pub fn u32(&mut self, t: u8, v: u32) -> &mut Builder {
        self.tlv(t, &v.to_le_bytes())
    }

    pub fn u64(&mut self, t: u8, v: u64) -> &mut Builder {
        self.tlv(t, &v.to_le_bytes())
    }

    /// The result TLV every response starts with.
    pub fn result(&mut self, result: u16, error: u16) -> &mut Builder {
        let mut v = [0u8; 4];
        v[..2].copy_from_slice(&result.to_le_bytes());
        v[2..].copy_from_slice(&error.to_le_bytes());
        self.tlv(RESULT_TLV, &v)
    }

    pub fn finish(&self, kind: u8, txn: u16, msg_id: u16) -> Vec<u8> {
        let mut out = self.buf.clone();
        let len = (out.len() - HEADER_LEN).min(usize::from(u16::MAX)) as u16;
        out[0] = kind;
        out[1..3].copy_from_slice(&txn.to_le_bytes());
        out[3..5].copy_from_slice(&msg_id.to_le_bytes());
        out[5..7].copy_from_slice(&len.to_le_bytes());
        out
    }
}

/// A string inside a struct: one length byte, then the bytes (libqrtr uses a one-byte length for strings of up to
/// 256 bytes).
pub fn put_inner_string(out: &mut Vec<u8>, s: &str) {
    let b = &s.as_bytes()[..s.len().min(255)];
    out.push(b.len() as u8);
    out.extend_from_slice(b);
}
