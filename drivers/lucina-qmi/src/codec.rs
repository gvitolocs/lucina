// SPDX-License-Identifier: GPL-2.0
//
// Derived from Linux drivers/soc/qcom/qmi_encdec.c (GPL-2.0, Copyright (c) 2012-2015 The Linux Foundation,
// Copyright (C) 2017 Linaro Ltd.) and include/linux/soc/qcom/qmi.h (GPL-2.0). The wire rules are the C's; the
// structure is not: Linux walks `struct qmi_elem_info` tables over raw struct offsets, here every message is a
// typed Rust struct that writes and reads itself with the helpers below.

//! The QMI wire format: the 7-byte header, TLVs, and the element encodings of `qmi_encdec.c`.
//!
//! * [`Header`] is `struct qmi_header`: type, transaction id, message id and payload length, little-endian,
//!   packed to 7 bytes.
//! * [`Writer`] is `qmi_encode()` at level 1. [`Writer::tlv`] opens one TLV (type byte, 16-bit length, value) and
//!   hands a [`Body`] for the value. [`Body`] writes what a table row would: integers and enums at their
//!   `elem_size`, variable-length arrays with the count prefix of `QMI_DATA_LEN` ([`LenWidth`]), nested structs
//!   as plain concatenation without TLV headers, and strings. A string is the whole TLV at the top level and
//!   gets a length prefix inside a struct (`qmi_encode_string_elem()`). Optional elements (`QMI_OPT_FLAG`) are
//!   `Option`s and are simply left out when `None`.
//! * [`Tlvs`] and [`Cursor`] are `qmi_decode()`: walk the TLVs, skip unknown optional ones (type 0x10 and up),
//!   refuse unknown mandatory ones, and read each known one with the same rules.
//! * [`Message`] ties a typed struct to a message id and a maximum length (`WLFW_*_MAX_MSG_LEN`);
//!   [`encode_message`] and [`decode_message`] are `qmi_encode_message()` and `qmi_decode_message()`.
//!
//! Where this is stricter than Linux, on purpose, because the input is hostile rather than trusted firmware:
//!
//! * The C never checks a TLV's length against the packet, so a TLV that claims more bytes than there are is
//!   skipped silently or read past the end. Here it is [`Error::Truncated`].
//! * The C ignores `msg_len` in the header and uses the packet length. Here they must agree
//!   ([`Error::LengthMismatch`]).
//! * The C accepts a struct that stops at a field boundary before its last field. Here every field must be
//!   there ([`Error::Truncated`]), and bytes left over in a TLV after its element are
//!   [`Error::TrailingBytes`] (the C reads them as the next TLV header).

use alloc::vec::Vec;
use core::fmt;

/// `sizeof(struct qmi_header)`.
pub const HEADER_LEN: usize = 7;
/// Type byte plus 16-bit length in front of every top-level element (`TLV_TYPE_SIZE + TLV_LEN_SIZE`).
pub const TLV_HEADER_LEN: usize = 3;
/// TLV types below this are mandatory (`OPTIONAL_TLV_TYPE_START`).
pub const OPTIONAL_TLV_TYPE_START: u8 = 0x10;
/// The TLV type of `struct qmi_response_type_v01` in every response that has one.
pub const RESPONSE_TLV_TYPE: u8 = 0x02;

pub const REQUEST: u8 = 0;
pub const RESPONSE: u8 = 2;
pub const INDICATION: u8 = 4;

pub const RESULT_SUCCESS: u16 = 0;
pub const RESULT_FAILURE: u16 = 1;

pub const ERR_NONE: u16 = 0;
pub const ERR_MALFORMED_MSG: u16 = 1;
pub const ERR_NO_MEMORY: u16 = 2;
pub const ERR_INTERNAL: u16 = 3;
pub const ERR_CLIENT_IDS_EXHAUSTED: u16 = 5;
pub const ERR_INVALID_ID: u16 = 41;
pub const ERR_ENCODING: u16 = 58;
pub const ERR_DISABLED: u16 = 69;
pub const ERR_INCOMPATIBLE_STATE: u16 = 90;
pub const ERR_NOT_SUPPORTED: u16 = 94;

/// Why an encode or decode was refused. The Linux errno the C returns for the same input is in brackets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Fewer than [`HEADER_LEN`] bytes. (The C callers check this before decoding.)
    ShortHeader,
    /// The header's `msg_len` is not the number of bytes after the header. Stricter than the C.
    LengthMismatch,
    /// A TLV header, a TLV value or an element inside one runs past its end. [EFAULT, or an unchecked read]
    Truncated,
    /// Bytes are left in a TLV after its element. [EFAULT for structs]
    TrailingBytes,
    /// A mandatory TLV (type below 0x10) the message does not have. [EINVAL]
    UnknownMandatoryTlv(u8),
    /// A variable-length array longer than its maximum. [ETOOSMALL when decoding, EINVAL when encoding]
    ArrayTooLong,
    /// A string longer than its buffer. Decoding refuses a length of `elem_len` or more, because the C
    /// stores a terminating NUL in the same buffer. [ETOOSMALL when decoding, EINVAL when encoding]
    StringTooLong,
    /// The encoded body is longer than the maximum the caller allowed. [ETOOSMALL]
    BufferTooSmall,
    /// One TLV, or the whole body, does not fit the 16-bit length field.
    TooLong,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::ShortHeader => f.write_str("shorter than a QMI header"),
            Error::LengthMismatch => f.write_str("QMI header length does not match the packet"),
            Error::Truncated => f.write_str("QMI data runs past its end"),
            Error::TrailingBytes => f.write_str("bytes left over after a QMI element"),
            Error::UnknownMandatoryTlv(t) => write!(f, "unknown mandatory TLV {t:#x}"),
            Error::ArrayTooLong => f.write_str("QMI array longer than its maximum"),
            Error::StringTooLong => f.write_str("QMI string longer than its buffer"),
            Error::BufferTooSmall => f.write_str("encoded QMI message longer than allowed"),
            Error::TooLong => f.write_str("QMI element longer than 65535 bytes"),
        }
    }
}

/// `struct qmi_header`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    /// [`REQUEST`], [`RESPONSE`] or [`INDICATION`].
    pub msg_type: u8,
    pub txn_id: u16,
    pub msg_id: u16,
    /// Length of the TLVs after the header.
    pub msg_len: u16,
}

impl Header {
    /// Split a packet into its header and the bytes after it. The length is not checked here.
    pub fn parse(buf: &[u8]) -> Result<(Header, &[u8]), Error> {
        let Some((head, body)) = buf.split_first_chunk::<HEADER_LEN>() else {
            return Err(Error::ShortHeader);
        };
        Ok((
            Header {
                msg_type: head[0],
                txn_id: u16::from_le_bytes([head[1], head[2]]),
                msg_id: u16::from_le_bytes([head[3], head[4]]),
                msg_len: u16::from_le_bytes([head[5], head[6]]),
            },
            body,
        ))
    }

    pub fn to_bytes(&self) -> [u8; HEADER_LEN] {
        let t = self.txn_id.to_le_bytes();
        let m = self.msg_id.to_le_bytes();
        let l = self.msg_len.to_le_bytes();
        [self.msg_type, t[0], t[1], m[0], m[1], l[0], l[1]]
    }
}

/// How wide the element count in front of a variable-length array is. The C picks it from the `elem_size` of
/// the `QMI_DATA_LEN` row: `sizeof(u8)` is one byte, anything else two.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LenWidth {
    U8,
    U16,
}

/// The prefix width of a string inside a struct: one byte when the buffer (`elem_len`) is at most 255, else two
/// (`qmi_encode_string_elem()`, `qmi_decode_string_elem()`).
fn string_prefix(elem_len: usize) -> LenWidth {
    if elem_len <= usize::from(u8::MAX) {
        LenWidth::U8
    } else {
        LenWidth::U16
    }
}

/// The bytes of a C string: up to the first NUL, as `strlen()` sees them.
fn c_str(s: &[u8]) -> &[u8] {
    match s.iter().position(|&b| b == 0) {
        Some(n) => &s[..n],
        None => s,
    }
}

/// Builds the TLVs of one message body.
#[derive(Debug)]
pub struct Writer {
    buf: Vec<u8>,
    limit: usize,
}

impl Writer {
    /// `limit` is the longest body allowed: the `len` the C passes to `qmi_encode_message()`, normally the
    /// message's `MAX_MSG_LEN`.
    pub fn new(limit: usize) -> Writer {
        Writer {
            buf: Vec::new(),
            limit,
        }
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.buf
    }

    /// One TLV whose value `f` writes.
    pub fn tlv<F>(&mut self, ty: u8, f: F) -> Result<(), Error>
    where
        F: FnOnce(&mut Body<'_>) -> Result<(), Error>,
    {
        let start = self.buf.len();
        self.buf.extend_from_slice(&[ty, 0, 0]);
        f(&mut Body { buf: &mut self.buf })?;
        let len = self.buf.len() - start - TLV_HEADER_LEN;
        let len = u16::try_from(len).map_err(|_| Error::TooLong)?;
        self.buf[start + 1..start + TLV_HEADER_LEN].copy_from_slice(&len.to_le_bytes());
        // The C checks before each element that header + data still fit; at the end of a TLV that is this.
        if self.buf.len() > self.limit {
            return Err(Error::BufferTooSmall);
        }
        Ok(())
    }

    /// An optional element: written only when `Some`. This is `QMI_OPT_FLAG` and its following row.
    pub fn opt_tlv<T, F>(&mut self, ty: u8, v: Option<&T>, f: F) -> Result<(), Error>
    where
        F: FnOnce(&mut Body<'_>, &T) -> Result<(), Error>,
    {
        match v {
            Some(v) => self.tlv(ty, |b| f(b, v)),
            None => Ok(()),
        }
    }

    pub fn tlv_u8(&mut self, ty: u8, v: u8) -> Result<(), Error> {
        self.tlv(ty, |b| {
            b.put_u8(v);
            Ok(())
        })
    }

    pub fn tlv_u32(&mut self, ty: u8, v: u32) -> Result<(), Error> {
        self.tlv(ty, |b| {
            b.put_u32(v);
            Ok(())
        })
    }

    pub fn tlv_u64(&mut self, ty: u8, v: u64) -> Result<(), Error> {
        self.tlv(ty, |b| {
            b.put_u64(v);
            Ok(())
        })
    }

    /// A 4-byte signed enum (`QMI_SIGNED_4_BYTE_ENUM`).
    pub fn tlv_i32(&mut self, ty: u8, v: i32) -> Result<(), Error> {
        self.tlv(ty, |b| {
            b.put_i32(v);
            Ok(())
        })
    }

    pub fn opt_u8(&mut self, ty: u8, v: Option<u8>) -> Result<(), Error> {
        self.opt_tlv(ty, v.as_ref(), |b, v| {
            b.put_u8(*v);
            Ok(())
        })
    }

    pub fn opt_u32(&mut self, ty: u8, v: Option<u32>) -> Result<(), Error> {
        self.opt_tlv(ty, v.as_ref(), |b, v| {
            b.put_u32(*v);
            Ok(())
        })
    }

    pub fn opt_u64(&mut self, ty: u8, v: Option<u64>) -> Result<(), Error> {
        self.opt_tlv(ty, v.as_ref(), |b, v| {
            b.put_u64(*v);
            Ok(())
        })
    }

    pub fn opt_i32(&mut self, ty: u8, v: Option<i32>) -> Result<(), Error> {
        self.opt_tlv(ty, v.as_ref(), |b, v| {
            b.put_i32(*v);
            Ok(())
        })
    }

    /// A string that is the whole TLV (level 1 in the C): no length prefix. `elem_len` is the C buffer size,
    /// which holds the terminating NUL, so at most `elem_len - 1` characters fit.
    pub fn tlv_str(&mut self, ty: u8, elem_len: usize, s: &[u8]) -> Result<(), Error> {
        self.tlv(ty, |b| b.put_str_bare(elem_len, s))
    }

    pub fn opt_str(&mut self, ty: u8, elem_len: usize, s: Option<&[u8]>) -> Result<(), Error> {
        self.opt_tlv(ty, s.as_ref(), |b, s| b.put_str_bare(elem_len, s))
    }

    /// A variable-length array that is the whole TLV: count prefix, then the elements.
    pub fn tlv_array<T, F>(
        &mut self,
        ty: u8,
        width: LenWidth,
        max: usize,
        items: &[T],
        f: F,
    ) -> Result<(), Error>
    where
        F: FnMut(&mut Body<'_>, &T) -> Result<(), Error>,
    {
        self.tlv(ty, |b| b.put_array(width, max, items, f))
    }

    pub fn opt_array<T, F>(
        &mut self,
        ty: u8,
        width: LenWidth,
        max: usize,
        items: Option<&[T]>,
        f: F,
    ) -> Result<(), Error>
    where
        F: FnMut(&mut Body<'_>, &T) -> Result<(), Error>,
    {
        match items {
            Some(items) => self.tlv_array(ty, width, max, items, f),
            None => Ok(()),
        }
    }
}

/// The value of the TLV being written.
#[derive(Debug)]
pub struct Body<'a> {
    buf: &'a mut Vec<u8>,
}

impl Body<'_> {
    pub fn put_u8(&mut self, v: u8) {
        self.buf.push(v);
    }

    pub fn put_u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    pub fn put_u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    pub fn put_u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    /// A 4-byte enum.
    pub fn put_i32(&mut self, v: i32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    /// Raw bytes with no prefix (a `STATIC_ARRAY` of `u8`).
    pub fn put_bytes(&mut self, v: &[u8]) {
        self.buf.extend_from_slice(v);
    }

    /// The count in front of a variable-length array (`QMI_DATA_LEN`). The C writes the low bits of the
    /// count; here a count above `max` is refused first, like the check on the array row that follows it.
    fn put_count(&mut self, width: LenWidth, max: usize, n: usize) -> Result<(), Error> {
        if n > max {
            return Err(Error::ArrayTooLong);
        }
        match width {
            // `max` is at most 255 for every one-byte count in the tables; keep the cast honest anyway.
            LenWidth::U8 => self.put_u8(u8::try_from(n).map_err(|_| Error::ArrayTooLong)?),
            LenWidth::U16 => self.put_u16(u16::try_from(n).map_err(|_| Error::ArrayTooLong)?),
        }
        Ok(())
    }

    /// A variable-length array: `QMI_DATA_LEN` then `QMI_*` with `VAR_LEN_ARRAY`.
    pub fn put_array<T, F>(
        &mut self,
        width: LenWidth,
        max: usize,
        items: &[T],
        mut f: F,
    ) -> Result<(), Error>
    where
        F: FnMut(&mut Body<'_>, &T) -> Result<(), Error>,
    {
        self.put_count(width, max, items.len())?;
        for item in items {
            f(self, item)?;
        }
        Ok(())
    }

    fn put_str_bare(&mut self, elem_len: usize, s: &[u8]) -> Result<(), Error> {
        let s = c_str(s);
        if s.len() >= elem_len {
            return Err(Error::StringTooLong);
        }
        self.buf.extend_from_slice(s);
        Ok(())
    }

    /// A string inside a struct: a length prefix (one byte when `elem_len <= 255`), then the characters.
    pub fn put_str(&mut self, elem_len: usize, s: &[u8]) -> Result<(), Error> {
        let s = c_str(s);
        if s.len() >= elem_len {
            return Err(Error::StringTooLong);
        }
        match string_prefix(elem_len) {
            LenWidth::U8 => self.put_u8(u8::try_from(s.len()).map_err(|_| Error::StringTooLong)?),
            LenWidth::U16 => {
                self.put_u16(u16::try_from(s.len()).map_err(|_| Error::StringTooLong)?)
            }
        }
        self.buf.extend_from_slice(s);
        Ok(())
    }
}

/// Reads one TLV's value, front to back.
#[derive(Debug, Clone)]
pub struct Cursor<'a> {
    data: &'a [u8],
}

impl<'a> Cursor<'a> {
    pub fn new(data: &'a [u8]) -> Cursor<'a> {
        Cursor { data }
    }

    pub fn remaining(&self) -> usize {
        self.data.len()
    }

    pub fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        match self.data.split_at_checked(n) {
            Some((head, rest)) => {
                self.data = rest;
                Ok(head)
            }
            None => Err(Error::Truncated),
        }
    }

    fn take_array<const N: usize>(&mut self) -> Result<[u8; N], Error> {
        match self.data.split_first_chunk::<N>() {
            Some((head, rest)) => {
                self.data = rest;
                Ok(*head)
            }
            None => Err(Error::Truncated),
        }
    }

    pub fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.take_array::<1>()?[0])
    }

    pub fn u16(&mut self) -> Result<u16, Error> {
        Ok(u16::from_le_bytes(self.take_array()?))
    }

    pub fn u32(&mut self) -> Result<u32, Error> {
        Ok(u32::from_le_bytes(self.take_array()?))
    }

    pub fn u64(&mut self) -> Result<u64, Error> {
        Ok(u64::from_le_bytes(self.take_array()?))
    }

    /// A 4-byte enum.
    pub fn i32(&mut self) -> Result<i32, Error> {
        Ok(i32::from_le_bytes(self.take_array()?))
    }

    fn count(&mut self, width: LenWidth) -> Result<usize, Error> {
        Ok(match width {
            LenWidth::U8 => usize::from(self.u8()?),
            LenWidth::U16 => usize::from(self.u16()?),
        })
    }

    /// A variable-length array: the count, refused above `max`, then that many elements.
    pub fn array<T, F>(&mut self, width: LenWidth, max: usize, mut f: F) -> Result<Vec<T>, Error>
    where
        F: FnMut(&mut Cursor<'a>) -> Result<T, Error>,
    {
        let n = self.count(width)?;
        if n > max {
            return Err(Error::ArrayTooLong);
        }
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            out.push(f(self)?);
        }
        Ok(out)
    }

    /// A string that is the rest of the TLV. The C refuses a length of `elem_len` or more.
    pub fn str_rest(&mut self, elem_len: usize) -> Result<Vec<u8>, Error> {
        let s = self.take(self.data.len())?;
        if s.len() >= elem_len {
            return Err(Error::StringTooLong);
        }
        Ok(c_str(s).to_vec())
    }

    /// A string inside a struct: the length prefix, then the characters.
    pub fn str_prefixed(&mut self, elem_len: usize) -> Result<Vec<u8>, Error> {
        let n = self.count(string_prefix(elem_len))?;
        if n >= elem_len {
            return Err(Error::StringTooLong);
        }
        Ok(c_str(self.take(n)?).to_vec())
    }

    /// The element is done: nothing may be left of the TLV.
    pub fn finish(&self) -> Result<(), Error> {
        if self.data.is_empty() {
            Ok(())
        } else {
            Err(Error::TrailingBytes)
        }
    }
}

/// The TLVs of a message body, in order.
#[derive(Debug, Clone)]
pub struct Tlvs<'a> {
    rest: &'a [u8],
}

impl<'a> Tlvs<'a> {
    pub fn new(body: &'a [u8]) -> Tlvs<'a> {
        Tlvs { rest: body }
    }
}

impl<'a> Iterator for Tlvs<'a> {
    type Item = Result<(u8, Cursor<'a>), Error>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.rest.is_empty() {
            return None;
        }
        let Some((head, after)) = self.rest.split_first_chunk::<TLV_HEADER_LEN>() else {
            self.rest = &[];
            return Some(Err(Error::Truncated));
        };
        let len = usize::from(u16::from_le_bytes([head[1], head[2]]));
        let Some((value, after)) = after.split_at_checked(len) else {
            self.rest = &[];
            return Some(Err(Error::Truncated));
        };
        self.rest = after;
        Some(Ok((head[0], Cursor::new(value))))
    }
}

/// A QMI message as a typed struct. `Default` is the all-zero, nothing-optional-present struct the C starts from
/// before it decodes.
pub trait Message: Sized + Default {
    /// `QMI_WLFW_*_V01`: requests, responses and indications of one operation share it.
    const MSG_ID: u16;
    /// `*_MAX_MSG_LEN`: the longest body the message can have.
    const MAX_LEN: usize;

    /// Write the TLVs in the order of the C element table.
    fn encode_body(&self, w: &mut Writer) -> Result<(), Error>;

    /// Read one known TLV into `self`. Returns `Ok(false)` for a type the message does not have.
    fn decode_tlv(&mut self, ty: u8, c: &mut Cursor<'_>) -> Result<bool, Error>;

    /// The TLVs after the header. Later copies of a TLV replace earlier ones, as in the C; missing ones leave
    /// the field at its default (the C does not check for mandatory TLVs either).
    fn decode_body(body: &[u8]) -> Result<Self, Error> {
        let mut m = Self::default();
        for tlv in Tlvs::new(body) {
            let (ty, mut c) = tlv?;
            if m.decode_tlv(ty, &mut c)? {
                c.finish()?;
            } else if ty < OPTIONAL_TLV_TYPE_START {
                return Err(Error::UnknownMandatoryTlv(ty));
            }
        }
        Ok(m)
    }
}

/// `qmi_encode_message()` with the message's own maximum length.
pub fn encode_message<M: Message>(msg_type: u8, txn_id: u16, m: &M) -> Result<Vec<u8>, Error> {
    encode_message_limited(msg_type, txn_id, m, M::MAX_LEN)
}

/// `qmi_encode_message()` with an explicit maximum body length.
pub fn encode_message_limited<M: Message>(
    msg_type: u8,
    txn_id: u16,
    m: &M,
    limit: usize,
) -> Result<Vec<u8>, Error> {
    let mut w = Writer::new(limit);
    m.encode_body(&mut w)?;
    let body = w.into_bytes();
    let msg_len = u16::try_from(body.len()).map_err(|_| Error::TooLong)?;
    let header = Header {
        msg_type,
        txn_id,
        msg_id: M::MSG_ID,
        msg_len,
    };
    let mut out = Vec::with_capacity(HEADER_LEN + body.len());
    out.extend_from_slice(&header.to_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

/// `qmi_decode_message()`. The message id in the header is not compared with `M::MSG_ID`; the caller picked
/// `M` from the transaction or the id already, exactly as the C picks `ei`.
pub fn decode_message<M: Message>(buf: &[u8]) -> Result<(Header, M), Error> {
    let (header, body) = Header::parse(buf)?;
    if usize::from(header.msg_len) != body.len() {
        return Err(Error::LengthMismatch);
    }
    Ok((header, M::decode_body(body)?))
}
