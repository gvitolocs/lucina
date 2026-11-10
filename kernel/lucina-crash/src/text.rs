//! Text in fixed buffers.

use core::fmt;

/// A `fmt::Write` into a slice: what does not fit is dropped, and writing never fails, so that a long message cannot
/// end a report.
pub struct Buf<'a> {
    out: &'a mut [u8],
    len: usize,
}

impl<'a> Buf<'a> {
    pub fn new(out: &'a mut [u8]) -> Self {
        Self { out, len: 0 }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.out[..self.len]
    }

    pub fn push(&mut self, bytes: &[u8]) {
        let n = bytes.len().min(self.out.len() - self.len);
        self.out[self.len..self.len + n].copy_from_slice(&bytes[..n]);
        self.len += n;
    }
}

impl fmt::Write for Buf<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.push(s.as_bytes());
        Ok(())
    }
}

/// Keeps the last bytes pushed into a slice.
pub struct Tail<'a> {
    buf: &'a mut [u8],
    head: usize,
    full: bool,
}

impl<'a> Tail<'a> {
    pub fn new(buf: &'a mut [u8]) -> Self {
        Self {
            buf,
            head: 0,
            full: false,
        }
    }

    pub fn push(&mut self, bytes: &[u8]) {
        if self.buf.is_empty() {
            return;
        }
        for &b in bytes {
            self.buf[self.head] = b;
            self.head += 1;
            if self.head == self.buf.len() {
                self.head = 0;
                self.full = true;
            }
        }
    }

    /// Turns the ring into plain text, oldest byte first, starting at a whole line if the start was overwritten.
    pub fn finish(self) -> &'a [u8] {
        let Self { buf, head, full } = self;
        if !full {
            return &buf[..head];
        }
        buf.rotate_left(head);
        let start = buf.iter().position(|&b| b == b'\n').map_or(0, |i| i + 1);
        &buf[start..]
    }
}

/// Adds the bytes of `text` to `tail` as plain printable ASCII: terminal escape sequences are dropped, other control
/// characters and anything beyond ASCII become `?`; newlines stay.
pub fn push_clean(tail: &mut Tail<'_>, text: &[u8]) {
    let mut bytes = text.iter().copied().peekable();
    while let Some(b) = bytes.next() {
        match b {
            0x1b => {
                // CSI: ESC [ parameters, then a final byte from 0x40 on. Any other sequence is ESC and one byte.
                if bytes.next() == Some(b'[') {
                    for c in bytes.by_ref() {
                        if (0x40..=0x7e).contains(&c) {
                            break;
                        }
                    }
                }
            }
            b'\n' | 0x20..=0x7e => tail.push(&[b]),
            b'\r' => {}
            _ => tail.push(b"?"),
        }
    }
}

/// Adds a /dev/kmsg record (`<level>,<sequence>,<timestamp in us>,<flags>;<escaped text>\n`) to `tail` as a line of the
/// console: `[    12.345678] text\n`. A record without that form is added as it is.
pub fn push_kmsg_record(tail: &mut Tail<'_>, record: &[u8]) {
    let record = match record.last() {
        Some(b'\n') => &record[..record.len() - 1],
        _ => record,
    };
    let Some(split) = record.iter().position(|&b| b == b';') else {
        push_clean(tail, record);
        tail.push(b"\n");
        return;
    };
    let (head, escaped) = (&record[..split], &record[split + 1..]);
    let us = head
        .split(|&b| b == b',')
        .nth(2)
        .and_then(|field| core::str::from_utf8(field).ok())
        .and_then(|field| field.parse::<u64>().ok());
    if let Some(us) = us {
        push_time(tail, us);
    }
    // The escapes of the record, undone in pieces so that an escape sequence for the terminal can be recognized.
    let mut line = [0u8; 256];
    let mut len = 0;
    let mut i = 0;
    while i < escaped.len() {
        let mut b = escaped[i];
        i += 1;
        if b == b'\\'
            && escaped.get(i) == Some(&b'x')
            && let (Some(hi), Some(lo)) = (
                escaped.get(i + 1).and_then(|c| hex_value(*c)),
                escaped.get(i + 2).and_then(|c| hex_value(*c)),
            )
        {
            b = hi << 4 | lo;
            i += 3;
        }
        line[len] = b;
        len += 1;
        if len == line.len() {
            // A sequence cut at this edge prints a little junk at worst.
            push_clean(tail, &line);
            len = 0;
        }
    }
    push_clean(tail, &line[..len]);
    tail.push(b"\n");
}

fn hex_value(c: u8) -> Option<u8> {
    (c as char).to_digit(16).map(|d| d as u8)
}

fn push_time(tail: &mut Tail<'_>, us: u64) {
    let mut scratch = [0u8; 40];
    let mut buf = Buf::new(&mut scratch);
    let _ = fmt::Write::write_fmt(
        &mut buf,
        format_args!("[{:>5}.{:06}] ", us / 1_000_000, us % 1_000_000),
    );
    tail.push(buf.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buf_drops_what_does_not_fit() {
        let mut out = [0u8; 4];
        let mut b = Buf::new(&mut out);
        fmt::Write::write_str(&mut b, "abcdef").unwrap();
        assert_eq!(b.as_bytes(), b"abcd");
    }

    #[test]
    fn tail_keeps_the_last_whole_lines() {
        let mut out = [0u8; 10];
        let mut t = Tail::new(&mut out);
        t.push(b"one\ntwo\nthree\n");
        assert_eq!(t.finish(), b"three\n");
    }

    #[test]
    fn tail_that_did_not_wrap_keeps_everything() {
        let mut out = [0u8; 10];
        let mut t = Tail::new(&mut out);
        t.push(b"one\ntwo");
        assert_eq!(t.finish(), b"one\ntwo");
    }

    #[test]
    fn clean_removes_colours_and_controls() {
        let mut out = [0u8; 64];
        let mut t = Tail::new(&mut out);
        push_clean(&mut t, b"\x1b[31mred\x1b[0m a\x07b\xc3\xa9\r\n");
        assert_eq!(t.finish(), b"red a?b??\n");
    }

    #[test]
    fn kmsg_record_becomes_a_console_line() {
        let mut out = [0u8; 128];
        let mut t = Tail::new(&mut out);
        push_kmsg_record(
            &mut t,
            b"6,12,1234567,-;\\x1b[32mINFO\\x1b[0m: back\\x5cslash\n",
        );
        push_kmsg_record(&mut t, b"not a record\n");
        assert_eq!(
            core::str::from_utf8(t.finish()).unwrap(),
            "[    1.234567] INFO: back\\slash\nnot a record\n"
        );
    }

    #[test]
    fn kmsg_record_longer_than_the_piece_buffer_stays_whole() {
        let mut out = [0u8; 2048];
        let mut t = Tail::new(&mut out);
        let mut record = [b'x'; 1024];
        record[..6].copy_from_slice(b"6,1,0;");
        push_kmsg_record(&mut t, &record);
        let text = t.finish();
        assert_eq!(text.iter().filter(|&&b| b == b'x').count(), 1018);
    }
}
