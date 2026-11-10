//! HTTP/1.1 as Fortuna speaks it to the gateway: one request at a time on a kept-alive TLS connection.
//!
//! - `head` writes a request's head (Host, the bearer when signed in, Content-Length).
//! - `Reader` takes the answer as it arrives, in pieces of any size: the status line, the headers it needs
//!   (Content-Length, Transfer-Encoding: chunked, Retry-After, Connection: close) and the body, with a hard cap
//!   (`rules::MAX_BODY`). Anything malformed is an error, never a guess.
//!
//! The bearer token goes into the head only; no other function sees it, and the head is never logged.

use alloc::string::String;
use alloc::vec::Vec;

use crate::rules;

/// What Fortuna tells the server it is (as the web app does, `X-Secretary-Client: web`, like Juno).
pub const CLIENT_CHANNEL: &str = "web";
const MAX_HEAD: usize = 8 * 1024;

/// A request's head. `token` is the device token (already checked by `rules::token_shape`).
#[allow(clippy::too_many_arguments)]
pub fn head(out: &mut Vec<u8>, method: &str, path: &str, host: &str, token: Option<&str>, content_type: Option<&str>, length: Option<u32>, version: &str) {
    let mut put = |s: &str| out.extend_from_slice(s.as_bytes());
    put(method);
    put(" ");
    put(path);
    put(" HTTP/1.1\r\nHost: ");
    put(host);
    put("\r\nUser-Agent: Fortuna/");
    put(version);
    put(" (Nintendo DSi)\r\nAccept: application/json\r\nX-Secretary-Client: ");
    put(CLIENT_CHANNEL);
    put("\r\nConnection: keep-alive\r\n");
    if let Some(t) = token {
        put("Authorization: Bearer ");
        put(t);
        put("\r\n");
    }
    if let Some(ct) = content_type {
        put("Content-Type: ");
        put(ct);
        put("\r\n");
    }
    if let Some(n) = length {
        put("Content-Length: ");
        let mut digits = [0u8; 10];
        put(dec(n, &mut digits));
        put("\r\n");
    }
    put("\r\n");
}

/// `n` in decimal, in `buf`.
pub fn dec(mut n: u32, buf: &mut [u8; 10]) -> &str {
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    core::str::from_utf8(&buf[i..]).unwrap_or("0")
}

/// Percent-encodes a query value or a path segment (everything but unreserved characters).
pub fn escape(s: &str, out: &mut String) {
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            const H: &[u8; 16] = b"0123456789ABCDEF";
            out.push('%');
            out.push(H[(b >> 4) as usize] as char);
            out.push(H[(b & 15) as usize] as char);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpError {
    /// Not HTTP/1.x, a header line without a colon, a bad number.
    Malformed,
    /// The head passed 8 KB or the body `rules::MAX_BODY`.
    TooLarge,
    /// The connection ended before the answer did.
    Truncated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Body {
    Length(u32),
    /// Chunk framing: the state of the size line or of the data.
    Chunked(Chunk),
    /// No length: the body runs to the end of the connection.
    ToClose,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Chunk {
    Size { acc: u32, digits: u32 },
    /// After the size: an extension until the end of the line.
    Ext { size: u32 },
    SizeLf { size: u32 },
    Data { left: u32 },
    DataCr,
    DataLf,
    /// After the last chunk: trailer lines until an empty one.
    Trailer { empty: bool },
    TrailerLf { empty: bool },
}

/// One answer.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Response {
    pub status: u16,
    pub retry_after: Option<u32>,
    /// The server will close the connection after this answer.
    pub close: bool,
    pub body: Vec<u8>,
}

/// Reads one answer from pieces of input.
pub struct Reader {
    head: Vec<u8>,
    head_done: bool,
    body: Body,
    resp: Response,
    done: bool,
    /// The answer to a HEAD request or a 204/304 has no body whatever the headers say.
    no_body: bool,
}

impl Default for Reader {
    fn default() -> Self {
        Reader::new(false)
    }
}

fn parse_u32(v: &[u8]) -> Option<u32> {
    if v.is_empty() || v.len() > 10 {
        return None;
    }
    let mut n: u64 = 0;
    for &b in v {
        if !b.is_ascii_digit() {
            return None;
        }
        n = n * 10 + (b - b'0') as u64;
    }
    u32::try_from(n).ok()
}

fn trim(mut v: &[u8]) -> &[u8] {
    while let [b' ' | b'\t', rest @ ..] = v {
        v = rest;
    }
    while let [rest @ .., b' ' | b'\t'] = v {
        v = rest;
    }
    v
}

impl Reader {
    pub fn new(no_body: bool) -> Reader {
        Reader { head: Vec::new(), head_done: false, body: Body::None, resp: Response::default(), done: false, no_body }
    }

    pub fn done(&self) -> bool {
        self.done
    }

    /// Takes the answer once `done`.
    pub fn finish(self) -> Response {
        self.resp
    }

    /// The connection ended: an answer that runs to the close is complete, any other is truncated.
    pub fn end(&mut self) -> Result<(), HttpError> {
        if self.done {
            return Ok(());
        }
        if self.head_done && self.body == Body::ToClose {
            self.done = true;
            self.resp.close = true;
            return Ok(());
        }
        Err(HttpError::Truncated)
    }

    /// Feeds input; returns how much of it belongs to this answer (the rest belongs to the next one).
    pub fn feed(&mut self, mut input: &[u8]) -> Result<usize, HttpError> {
        let total = input.len();
        while !input.is_empty() && !self.done {
            if !self.head_done {
                let used = self.feed_head(input)?;
                input = &input[used..];
                continue;
            }
            let used = self.feed_body(input)?;
            input = &input[used..];
        }
        Ok(total - input.len())
    }

    fn feed_head(&mut self, input: &[u8]) -> Result<usize, HttpError> {
        // Byte by byte up to the empty line, so that the body that follows is never taken as head.
        for (i, &b) in input.iter().enumerate() {
            self.head.push(b);
            if self.head.len() > MAX_HEAD {
                return Err(HttpError::TooLarge);
            }
            if self.head.ends_with(b"\r\n\r\n") {
                let interim = self.parse_head()?;
                self.head = Vec::new();
                if interim {
                    return Ok(i + 1);
                }
                self.head_done = true;
                if self.body == Body::None || self.body == Body::Length(0) {
                    self.done = true;
                }
                return Ok(i + 1);
            }
        }
        Ok(input.len())
    }

    fn parse_head(&mut self) -> Result<bool, HttpError> {
        let head = core::mem::take(&mut self.head);
        let mut lines = head[..head.len() - 4].split(|&b| b == b'\n').map(|l| l.strip_suffix(b"\r").unwrap_or(l));
        let status = lines.next().ok_or(HttpError::Malformed)?;
        if !(status.starts_with(b"HTTP/1.1 ") || status.starts_with(b"HTTP/1.0 ")) || status.len() < 12 {
            return Err(HttpError::Malformed);
        }
        let code = parse_u32(&status[9..12]).ok_or(HttpError::Malformed)?;
        if !(100..=599).contains(&code) || status.get(12).is_some_and(|&b| b != b' ') {
            return Err(HttpError::Malformed);
        }
        self.resp.status = code as u16;
        let mut length: Option<u32> = None;
        let mut chunked = false;
        for line in lines {
            let colon = line.iter().position(|&b| b == b':').ok_or(HttpError::Malformed)?;
            let name = &line[..colon];
            let value = trim(&line[colon + 1..]);
            if name.eq_ignore_ascii_case(b"content-length") {
                let n = parse_u32(value).ok_or(HttpError::Malformed)?;
                if length.is_some_and(|l| l != n) {
                    return Err(HttpError::Malformed);
                }
                if n > rules::MAX_BODY {
                    return Err(HttpError::TooLarge);
                }
                length = Some(n);
            } else if name.eq_ignore_ascii_case(b"transfer-encoding") {
                if value.eq_ignore_ascii_case(b"chunked") {
                    chunked = true;
                } else if !value.eq_ignore_ascii_case(b"identity") {
                    return Err(HttpError::Malformed);
                }
            } else if name.eq_ignore_ascii_case(b"retry-after") {
                self.resp.retry_after = parse_u32(value);
            } else if name.eq_ignore_ascii_case(b"connection") && value.eq_ignore_ascii_case(b"close") {
                self.resp.close = true;
            }
        }
        // RFC 9112 6.3: a 1xx, 204 and 304 (and HEAD) have no body; chunked wins over a length, and both is suspicious.
        self.body = if self.no_body || code < 200 || code == 204 || code == 304 {
            Body::None
        } else if chunked {
            if length.is_some() {
                return Err(HttpError::Malformed);
            }
            Body::Chunked(Chunk::Size { acc: 0, digits: 0 })
        } else if let Some(n) = length {
            Body::Length(n)
        } else {
            self.resp.close = true;
            Body::ToClose
        };
        if code < 200 && code != 101 {
            // An interim answer (100 Continue): read the real one next.
            self.resp = Response::default();
            self.body = Body::None;
            return Ok(true);
        }
        Ok(false)
    }

    fn take(&mut self, remaining: u32, input: &[u8]) -> Result<u32, HttpError> {
        let avail = input.len().min(u32::MAX as usize) as u32;
        let held = self.resp.body.len() as u32;
        let n = rules::body_take(remaining, avail, held, rules::MAX_BODY).ok_or(HttpError::TooLarge)?;
        self.resp.body.extend_from_slice(&input[..n as usize]);
        Ok(n)
    }

    fn feed_body(&mut self, input: &[u8]) -> Result<usize, HttpError> {
        match self.body {
            Body::None => {
                self.done = true;
                Ok(0)
            }
            Body::Length(left) => {
                let n = self.take(left, input)?;
                self.body = Body::Length(left - n);
                if left == n {
                    self.done = true;
                }
                Ok(n as usize)
            }
            Body::ToClose => {
                let n = self.take(u32::MAX, input)?;
                Ok(n as usize)
            }
            Body::Chunked(c) => self.feed_chunk(c, input),
        }
    }

    fn feed_chunk(&mut self, c: Chunk, input: &[u8]) -> Result<usize, HttpError> {
        let b = input[0];
        let next = match c {
            Chunk::Size { acc, digits } => {
                let v = rules::hex_value(b);
                if v < 16 {
                    Chunk::Size { acc: rules::push_hex(acc, v).ok_or(HttpError::TooLarge)?, digits: digits + 1 }
                } else if digits == 0 {
                    return Err(HttpError::Malformed);
                } else if b == b';' || b == b' ' || b == b'\t' {
                    Chunk::Ext { size: acc }
                } else if b == b'\r' {
                    Chunk::SizeLf { size: acc }
                } else {
                    return Err(HttpError::Malformed);
                }
            }
            Chunk::Ext { size } => {
                if b == b'\r' {
                    Chunk::SizeLf { size }
                } else if b == b'\n' {
                    return Err(HttpError::Malformed);
                } else {
                    Chunk::Ext { size }
                }
            }
            Chunk::SizeLf { size } => {
                if b != b'\n' {
                    return Err(HttpError::Malformed);
                }
                if size == 0 {
                    Chunk::Trailer { empty: true }
                } else {
                    Chunk::Data { left: size }
                }
            }
            Chunk::Data { left } => {
                let n = self.take(left, input)?;
                self.body = Body::Chunked(if n == left { Chunk::DataCr } else { Chunk::Data { left: left - n } });
                return Ok(n as usize);
            }
            Chunk::DataCr => {
                if b != b'\r' {
                    return Err(HttpError::Malformed);
                }
                Chunk::DataLf
            }
            Chunk::DataLf => {
                if b != b'\n' {
                    return Err(HttpError::Malformed);
                }
                Chunk::Size { acc: 0, digits: 0 }
            }
            Chunk::Trailer { empty } => {
                if b == b'\r' {
                    Chunk::TrailerLf { empty }
                } else {
                    Chunk::Trailer { empty: false }
                }
            }
            Chunk::TrailerLf { empty } => {
                if b != b'\n' {
                    return Err(HttpError::Malformed);
                }
                if empty {
                    self.done = true;
                    return Ok(1);
                }
                Chunk::Trailer { empty: true }
            }
        };
        self.body = Body::Chunked(next);
        Ok(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn read_all(pieces: &[&[u8]]) -> Result<(Response, usize), HttpError> {
        let mut r = Reader::new(false);
        let mut used = 0;
        for p in pieces {
            used += r.feed(p)?;
            if r.done() {
                break;
            }
        }
        if !r.done() {
            r.end()?;
        }
        Ok((r.finish(), used))
    }

    #[test]
    fn a_length_answer_in_every_split() {
        let a = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 11\r\n\r\n{\"ok\":true}EXTRA";
        for cut in 0..a.len() {
            let (r, used) = read_all(&[&a[..cut], &a[cut..]]).unwrap();
            assert_eq!(r.status, 200);
            assert_eq!(r.body, b"{\"ok\":true}");
            assert_eq!(used, a.len() - 5, "cut {cut}");
        }
    }

    #[test]
    fn chunked_byte_by_byte_with_extensions_and_trailers() {
        let a = b"HTTP/1.1 201 Created\r\ntransfer-encoding: Chunked\r\nRetry-After: 7\r\n\r\n4;x=y\r\nWiki\r\n5\r\npedia\r\nE\r\n in\r\n\r\nchunks.\r\n0\r\nX-T: 1\r\n\r\nNEXT";
        let pieces: Vec<&[u8]> = a.chunks(1).collect();
        let (r, used) = read_all(&pieces).unwrap();
        assert_eq!(r.status, 201);
        assert_eq!(r.retry_after, Some(7));
        assert_eq!(r.body, b"Wikipedia in\r\n\r\nchunks.");
        assert_eq!(used, a.len() - 4);
    }

    #[test]
    fn bad_and_huge_answers_are_refused() {
        assert_eq!(read_all(&[b"HTTP/2 200\r\n\r\n"]).unwrap_err(), HttpError::Malformed);
        assert_eq!(read_all(&[b"HTTP/1.1 200 OK\r\nContent-Length: 9999999\r\n\r\n"]).unwrap_err(), HttpError::TooLarge);
        assert_eq!(read_all(&[b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\n"]).unwrap_err(), HttpError::Malformed);
        assert_eq!(read_all(&[b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nFFFFFFFFF\r\n"]).unwrap_err(), HttpError::TooLarge);
        assert_eq!(read_all(&[b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n"]).unwrap_err(), HttpError::Malformed);
        assert_eq!(read_all(&[b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Length: 3\r\n\r\n"]).unwrap_err(), HttpError::Malformed);
        assert_eq!(read_all(&[b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nab"]).unwrap_err(), HttpError::Truncated);
        let big = vec![b'a'; 9000];
        assert_eq!(read_all(&[b"HTTP/1.1 200 OK\r\nX: ", &big]).unwrap_err(), HttpError::TooLarge);
    }

    #[test]
    fn no_length_runs_to_the_close_and_204_has_no_body() {
        let (r, _) = read_all(&[b"HTTP/1.0 200 OK\r\n\r\nhello"]).unwrap();
        assert_eq!(r.body, b"hello");
        assert!(r.close);
        let (r, used) = read_all(&[b"HTTP/1.1 204 No Content\r\nContent-Length: 4\r\n\r\nabcd"]).unwrap();
        assert!(r.body.is_empty());
        assert_eq!(used, 46);
        let (r, _) = read_all(&[b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok"]).unwrap();
        assert_eq!((r.status, r.body.as_slice()), (200, &b"ok"[..]));
    }

    #[test]
    fn heads() {
        let mut v = Vec::new();
        head(&mut v, "POST", "/v1/chat", "getsecretary.tech", Some("sec_x"), Some("application/json"), Some(12), "1.54");
        let s = core::str::from_utf8(&v).unwrap();
        assert!(s.starts_with("POST /v1/chat HTTP/1.1\r\nHost: getsecretary.tech\r\n"));
        assert!(s.contains("Authorization: Bearer sec_x\r\n"));
        assert!(s.contains("Content-Length: 12\r\n"));
        assert!(s.ends_with("\r\n\r\n"));
        let mut e = String::new();
        escape("a b/ü?", &mut e);
        assert_eq!(e, "a%20b%2F%C3%BC%3F");
        let mut d = [0u8; 10];
        assert_eq!(dec(0, &mut d), "0");
        assert_eq!(dec(u32::MAX, &mut d), "4294967295");
    }
}
