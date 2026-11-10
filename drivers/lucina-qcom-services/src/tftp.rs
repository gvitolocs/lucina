// SPDX-License-Identifier: BSD-3-Clause
//
// From userspace/crates/lucina-qcom/src/tftp.rs, after linux-msm tqftpserv's tqftpserv.c (Copyright (c) 2018, Linaro
// Ltd.).

//! TFTP on QRTR, as linux-msm tqftpserv speaks it: RFC 1350 with the option extensions (RFC 2347) the modem uses:
//! `blksize`, `tsize`, `timeoutms`, `wsize` (blocks sent per ACK), and two of Qualcomm's own, `rsize` (read only
//! this many bytes) and `seek` (start at this offset).
//!
//! Reads with `rsize` produce exactly the C server's packets, including its quirks (an `rsize` that is a multiple
//! of `blksize` gets one more full block; blocks past the end of the file are sent empty): that is what the modem
//! firmware was tested against. `tests/c_oracle.rs` compares the two.
//!
//! Differences from the C original, on purpose, as in the userspace crate:
//! * a read without `rsize` sends the whole file (RFC 1350); the C server sends one block and then closes;
//! * a write request without options is answered with ACK 0 (RFC 1350).
//!
//! Different from the userspace crate: [`Reader::next_data`] hands out one block at a time rather than a window at
//! once, because a window can ask for 65535 blocks of 65464 bytes and the router's flow control may stop it after
//! ten.

use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use crate::IoError;

/// Opcodes.
pub const RRQ: u16 = 1;
pub const WRQ: u16 = 2;
pub const DATA: u16 = 3;
pub const ACK: u16 = 4;
pub const ERROR: u16 = 5;
pub const OACK: u16 = 6;

/// Error codes (RFC 1350, plus Qualcomm's 9).
pub const ERR_UNDEF: u16 = 0;
pub const ERR_ENOENT: u16 = 1;
pub const ERR_EACCESS: u16 = 2;
pub const ERR_ENOSPACE: u16 = 3;
pub const ERR_EBADOP: u16 = 4;
pub const ERR_EOPTNEG: u16 = 8;
/// "End of transfer": the client got what it wanted (a stat-like request).
pub const ERR_END_OF_TRANSFER: u16 = 9;

/// The option ranges tqftpserv enforces.
const BLKSIZE: (i64, i64) = (8, 65464);
const WSIZE: (i64, i64) = (1, 65535);
const RSIZE: (i64, i64) = (1, 100 * 1024 * 1024);
const TIMEOUTMS: (i64, i64) = (1, 255_000);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Read,
    Write,
}

/// Transfer options, with tqftpserv's defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    pub blksize: usize,
    pub timeoutms: u32,
    /// Asked for (`Some`): a read answers with the file size, a write echoes it.
    pub tsize: Option<u64>,
    pub wsize: usize,
    /// 0: not asked for.
    pub rsize: usize,
    pub seek: Option<u64>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            blksize: 512,
            timeoutms: 1000,
            tsize: None,
            wsize: 1,
            rsize: 0,
            seek: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub op: Op,
    pub filename: String,
    pub opts: Options,
    /// Whether the request carried options: then the server answers with OACK (C: `do_oack`).
    pub has_options: bool,
}

/// Why a request is refused, and what (if anything) the client is told.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// Truncated or unterminated: dropped without an answer.
    Malformed,
    /// An ERROR packet to send back.
    Error(u16, &'static str),
}

/// Parse an RRQ or WRQ.
pub fn parse_request(buf: &[u8]) -> Result<Request, Refusal> {
    let op = match opcode(buf) {
        Some(RRQ) => Op::Read,
        Some(WRQ) => Op::Write,
        _ => return Err(Refusal::Malformed),
    };
    let mut fields = Fields { rest: &buf[2..] };
    let filename = fields.next().ok_or(Refusal::Malformed)?;
    let mode = fields.next().ok_or(Refusal::Malformed)?;
    if !mode.eq_ignore_ascii_case("octet") {
        return Err(Refusal::Error(ERR_EBADOP, "Only octet mode supported"));
    }
    if !crate::translate::sanitize(&filename) {
        return Err(Refusal::Error(ERR_EACCESS, "Access violation"));
    }
    let has_options = !fields.rest.is_empty();
    let opts = if has_options {
        parse_options(fields.rest)
            .ok_or(Refusal::Error(ERR_EOPTNEG, "Option negotiation failed"))?
    } else {
        Options::default()
    };
    Ok(Request {
        op,
        filename,
        opts,
        has_options,
    })
}

/// NUL-terminated strings, one after another.
struct Fields<'a> {
    rest: &'a [u8],
}

impl Fields<'_> {
    fn next(&mut self) -> Option<String> {
        let end = self.rest.iter().position(|&b| b == 0)?;
        let s = String::from_utf8_lossy(&self.rest[..end]).into_owned();
        self.rest = &self.rest[end + 1..];
        Some(s)
    }
}

/// tqftpserv's `parse_options()`: name/value pairs; a known option with a bad value fails the whole request,
/// unknown options are ignored.
pub fn parse_options(buf: &[u8]) -> Option<Options> {
    let mut o = Options::default();
    let mut f = Fields { rest: buf };
    while !f.rest.is_empty() {
        let name = f.next()?;
        if f.rest.is_empty() {
            return None;
        }
        let value = f.next()?;
        let num = || c_strtoll(&value);
        let ranged = |(lo, hi): (i64, i64)| num().filter(|v| (lo..=hi).contains(v));
        match name.as_str() {
            "blksize" => o.blksize = ranged(BLKSIZE)? as usize,
            "timeoutms" => o.timeoutms = ranged(TIMEOUTMS)? as u32,
            "tsize" => o.tsize = Some(num().filter(|v| *v >= 0)? as u64),
            "rsize" => o.rsize = ranged(RSIZE)? as usize,
            "wsize" => o.wsize = ranged(WSIZE)? as usize,
            "seek" => o.seek = Some(num().filter(|v| *v >= 0)? as u64),
            _ => {}
        }
    }
    Some(o)
}

/// A whole value as `strtoll(value, &end, 10)` with `*end == '\0'` accepts it: leading C white space and a sign are
/// allowed, and an empty string is 0 (no digits leave `end` at the start, which is the terminator).
fn c_strtoll(value: &str) -> Option<i64> {
    if value.is_empty() {
        return Some(0);
    }
    value
        .trim_start_matches([' ', '\t', '\n', '\x0b', '\x0c', '\r'])
        .parse()
        .ok()
}

pub fn opcode(buf: &[u8]) -> Option<u16> {
    (buf.len() >= 2).then(|| u16::from_be_bytes([buf[0], buf[1]]))
}

/// The OACK tqftpserv sends: blksize, timeoutms, tsize (when asked), wsize, rsize (when not 0), seek (when asked),
/// in that order.
pub fn oack(o: &Options) -> Vec<u8> {
    let mut v = OACK.to_be_bytes().to_vec();
    let mut opt = |name: &str, value: String| {
        v.extend_from_slice(name.as_bytes());
        v.push(0);
        v.extend_from_slice(value.as_bytes());
        v.push(0);
    };
    opt("blksize", o.blksize.to_string());
    opt("timeoutms", o.timeoutms.to_string());
    if let Some(t) = o.tsize {
        opt("tsize", t.to_string());
    }
    opt("wsize", o.wsize.to_string());
    if o.rsize != 0 {
        opt("rsize", o.rsize.to_string());
    }
    if let Some(s) = o.seek {
        opt("seek", s.to_string());
    }
    v
}

pub fn ack(block: u16) -> [u8; 4] {
    let mut a = [0u8; 4];
    a[..2].copy_from_slice(&ACK.to_be_bytes());
    a[2..].copy_from_slice(&block.to_be_bytes());
    a
}

pub fn error(code: u16, msg: &str) -> Vec<u8> {
    let mut v = ERROR.to_be_bytes().to_vec();
    v.extend_from_slice(&code.to_be_bytes());
    v.extend_from_slice(msg.as_bytes());
    v.push(0);
    v
}

pub fn data(block: u16, payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(4 + payload.len());
    v.extend_from_slice(&DATA.to_be_bytes());
    v.extend_from_slice(&block.to_be_bytes());
    v.extend_from_slice(payload);
    v
}

/// A block number from a DATA or ACK packet, or the error code of an ERROR.
pub fn block_of(buf: &[u8]) -> Option<u16> {
    (buf.len() >= 4).then(|| u16::from_be_bytes([buf[2], buf[3]]))
}

/// What an ACK means for a read transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AckStep {
    /// A new window: take its blocks with [`Reader::next_data`].
    Window,
    /// The transfer is over: close it.
    Done,
}

/// One read transfer.
#[derive(Debug, Clone)]
pub struct Reader {
    pub opts: Options,
    /// Without rsize: the number of the short block that ended the file, once it was sent.
    last_block: Option<u16>,
    /// The window: blocks `next + 1 ..= end` remain to send.
    next: u32,
    end: u32,
}

impl Reader {
    pub fn new(opts: Options) -> Reader {
        Reader {
            opts,
            last_block: None,
            next: 0,
            end: 0,
        }
    }

    /// A request without options: block 1 goes out at once, no ACK needed.
    pub fn start_first(&mut self) {
        self.next = 0;
        self.end = 1;
    }

    /// The client acknowledged block `last` (0 after an OACK). tqftpserv's `handle_reader()`, quirks included.
    pub fn on_ack(&mut self, last: u16) -> AckStep {
        let done = if self.opts.rsize != 0 {
            u64::from(last) * self.opts.blksize as u64 > self.opts.rsize as u64
        } else {
            self.last_block.is_some_and(|b| last >= b)
        };
        if done {
            return AckStep::Done;
        }
        self.next = u32::from(last);
        self.end = u32::from(last) + self.opts.wsize as u32;
        AckStep::Window
    }

    /// The next DATA packet of the window, if any. `read_at(offset, buf)` reads the file like `pread(2)`. Err: the
    /// read failed and the transfer ends with an ERROR. With `rsize` a failed read ends only this window and the
    /// client stays (C: logs "Sent block failed").
    pub fn next_data<F>(&mut self, mut read_at: F) -> Result<Option<Vec<u8>>, IoError>
    where
        F: FnMut(u64, &mut [u8]) -> Result<usize, IoError>,
    {
        if self.next >= self.end || self.next >= u32::from(u16::MAX) {
            return Ok(None);
        }
        let block = self.next as u16 + 1;
        let bs = self.opts.blksize as u64;
        let rsize = self.opts.rsize as u64;
        if rsize != 0 {
            let crosses = u64::from(block) * bs > rsize;
            let limit = if crosses { (rsize % bs) as usize } else { 0 };
            match self.block(block, limit, &mut read_at) {
                Ok((pkt, _)) => {
                    self.next += 1;
                    if crosses {
                        self.end = self.next;
                    }
                    Ok(Some(pkt))
                }
                Err(_) => {
                    self.end = self.next;
                    Ok(None)
                }
            }
        } else {
            let (pkt, short) = self.block(block, 0, &mut read_at)?;
            self.next += 1;
            if short {
                self.last_block = Some(block);
                self.end = self.next;
            }
            Ok(Some(pkt))
        }
    }

    /// DATA for block `block` (1-based), cut to `limit` bytes when not 0; and whether it came out shorter than a
    /// block.
    fn block<F>(
        &self,
        block: u16,
        limit: usize,
        read_at: &mut F,
    ) -> Result<(Vec<u8>, bool), IoError>
    where
        F: FnMut(u64, &mut [u8]) -> Result<usize, IoError>,
    {
        let bs = self.opts.blksize;
        let offset = self
            .opts
            .seek
            .unwrap_or(0)
            .saturating_add(u64::from(block - 1) * bs as u64);
        let mut out = vec![0u8; 4 + bs];
        out[..2].copy_from_slice(&DATA.to_be_bytes());
        out[2..4].copy_from_slice(&block.to_be_bytes());
        let mut n = 0;
        while n < bs {
            let got = read_at(offset.saturating_add(n as u64), &mut out[4 + n..])?;
            if got == 0 {
                break;
            }
            n += got.min(bs - n);
        }
        if limit != 0 {
            if n < limit {
                return Err(IoError);
            }
            n = limit;
        }
        out.truncate(4 + n);
        Ok((out, n < bs))
    }
}

/// One write transfer (tqftpserv's current `handle_writer()`: blocks are collected per window and stored when the
/// window is full or a short block ends the file; each store is acknowledged). A block out of sequence ends the
/// transfer with an ERROR, as it does in the C server.
#[derive(Debug, Clone)]
pub struct Writer {
    pub opts: Options,
    /// The most a window may hold: the C has the disk, here a hostile window must not fill the kernel's memory.
    limit: usize,
    expected: u16,
    pending: Vec<u8>,
}

/// What a write transfer does with one DATA packet.
#[derive(Debug, PartialEq, Eq)]
pub enum WriteStep {
    /// Store these bytes, then send this ACK; `done` ends it.
    Flush {
        bytes: Vec<u8>,
        ack: [u8; 4],
        done: bool,
    },
    /// Keep collecting.
    More,
    /// Send this ERROR and drop the transfer.
    Abort(Vec<u8>),
}

impl Writer {
    pub fn new(opts: Options, limit: usize) -> Writer {
        Writer {
            opts,
            limit,
            expected: 1,
            pending: Vec::new(),
        }
    }

    pub fn on_packet(&mut self, buf: &[u8]) -> WriteStep {
        if buf.len() < 4 {
            return WriteStep::Abort(error(ERR_EBADOP, "Malformed packet"));
        }
        if opcode(buf) != Some(DATA) {
            return WriteStep::Abort(error(ERR_EBADOP, "Expected DATA opcode"));
        }
        let block = block_of(buf).unwrap_or(0);
        if block != self.expected {
            return WriteStep::Abort(error(ERR_EBADOP, "Block number out of sequence"));
        }
        let payload = &buf[4..];
        if self.pending.len().saturating_add(payload.len()) > self.limit {
            return WriteStep::Abort(error(ERR_ENOSPACE, "Disk full or write error"));
        }
        self.expected = self.expected.wrapping_add(1);
        self.pending.extend_from_slice(payload);
        let short = payload.len() < self.opts.blksize;
        if usize::from(block) % self.opts.wsize.max(1) == 0 || short {
            WriteStep::Flush {
                bytes: core::mem::take(&mut self.pending),
                ack: ack(block),
                done: short,
            }
        } else {
            WriteStep::More
        }
    }
}
