//! The kernel log's ring of records (kernel patch 0500).
//!
//! Text comes in through [`KmsgRing::record`] and [`KmsgRing::text`] from any context, split into one record per line,
//! and readers walk the records with a [`Cursor`] of their own. Old records are overwritten; a reader that was
//! overtaken learns it through [`Read::Overrun`] and goes on from the oldest record, as /dev/kmsg's EPIPE on Linux.
//!
//! [`KmsgRing::read`] renders a record in Linux's /dev/kmsg format (`Documentation/ABI/testing/dev-kmsg`):
//! `<level>,<sequence>,<timestamp in us>,-;<text>\n`, with bytes below 0x20, from 0x7f and the backslash written as
//! `\xNN`. The facility is always 0 (kernel), so the first field is the level alone.
#![no_std]
#![forbid(unsafe_code)]

use core::fmt::{self, Write};

/// The longest line kept, in bytes; the rest of a longer line is dropped.
pub const LINE_MAX: usize = 1024;

/// A buffer of this size holds any record [`KmsgRing::read`] renders: two 20-digit numbers, the level, the punctuation
/// and the text with every byte escaped.
pub const RECORD_MAX: usize = 64 + 4 * LINE_MAX;

/// Each record in the ring: timestamp (8 bytes, LE), level (1), text length (2, LE), text.
const HEADER: usize = 11;

/// The level of printed text: kernel prints that are not log records.
pub const LEVEL_INFO: u8 = 6;

/// A reader's position: the next record to read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Cursor {
    seq: u64,
    offset: usize,
}

/// What [`KmsgRing::read`] found.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Read {
    /// No record after the cursor yet.
    Empty,
    /// The cursor's record was overwritten; `oldest` is where to go on.
    Overrun { oldest: Cursor },
    /// `len` bytes were rendered; `next` is the cursor for the following record.
    Record { len: usize, next: Cursor },
}

/// The ring. `N` is its size in bytes; all of its fields start as zero, so that a `static` of it stays in the BSS.
pub struct KmsgRing<const N: usize> {
    buf: [u8; N],
    head: usize,
    used: usize,
    first_seq: u64,
    next_seq: u64,
    line: [u8; LINE_MAX],
    line_len: usize,
    line_level: u8,
    now_us: u64,
}

impl<const N: usize> KmsgRing<N> {
    pub const fn new() -> Self {
        Self {
            buf: [0; N],
            head: 0,
            used: 0,
            first_seq: 0,
            next_seq: 0,
            line: [0; LINE_MAX],
            line_len: 0,
            line_level: 0,
            now_us: 0,
        }
    }

    /// Adds one log record: whatever line was open is closed first, and this one is closed at the end.
    pub fn record(&mut self, level: u8, now_us: u64, prefix: &str, args: fmt::Arguments) {
        self.end_line();
        self.now_us = now_us;
        self.line_level = level;
        // The sink itself never fails; a failing `Display` keeps what it wrote.
        let _ = self.write_str(prefix);
        let _ = self.write_fmt(args);
        self.end_line();
    }

    /// Adds printed text, which may come in pieces: a line is complete at its newline.
    pub fn text(&mut self, now_us: u64, args: fmt::Arguments) {
        self.now_us = now_us;
        self.line_level = LEVEL_INFO;
        let _ = self.write_fmt(args);
    }

    /// The cursor of the oldest record.
    pub fn start(&self) -> Cursor {
        Cursor {
            seq: self.first_seq,
            offset: self.head,
        }
    }

    /// Renders the record at `cursor` into `out`, which should hold [`RECORD_MAX`] bytes (a record that does not fit
    /// is cut). The cursor stays valid until its record is overwritten, which this notices by the sequence number.
    pub fn read(&self, cursor: Cursor, out: &mut [u8]) -> Read {
        if cursor.seq < self.first_seq {
            return Read::Overrun {
                oldest: self.start(),
            };
        }
        if cursor.seq >= self.next_seq {
            return Read::Empty;
        }
        let at = |i: usize| self.buf[(cursor.offset + i) % N];
        let mut now = [0; 8];
        now.iter_mut().enumerate().for_each(|(i, b)| *b = at(i));
        let level = at(8);
        let len = u16::from_le_bytes([at(9), at(10)]) as usize;

        let mut sink = Sink { out, len: 0 };
        let _ = write!(
            sink,
            "{},{},{},-;",
            level,
            cursor.seq,
            u64::from_le_bytes(now)
        );
        for i in 0..len {
            let c = at(HEADER + i);
            if !(0x20..0x7f).contains(&c) || c == b'\\' {
                let _ = write!(sink, "\\x{:02x}", c);
            } else {
                sink.push(c);
            }
        }
        sink.push(b'\n');
        Read::Record {
            len: sink.len,
            next: Cursor {
                seq: cursor.seq + 1,
                offset: (cursor.offset + HEADER + len) % N,
            },
        }
    }

    /// Closes the open line, if it has anything in it. Empty lines are not kept.
    fn end_line(&mut self) {
        let len = core::mem::take(&mut self.line_len);
        let total = HEADER + len;
        if len == 0 || total > N {
            return;
        }
        while self.used + total > N {
            let at = |i: usize| self.buf[(self.head + i) % N];
            let old = HEADER + u16::from_le_bytes([at(9), at(10)]) as usize;
            self.head = (self.head + old) % N;
            self.used -= old;
            self.first_seq += 1;
        }
        let mut offset = (self.head + self.used) % N;
        let now = self.now_us.to_le_bytes();
        let length = (len as u16).to_le_bytes();
        let header = now.iter().chain([&self.line_level]).chain(&length);
        for &b in header.chain(&self.line[..len]) {
            self.buf[offset] = b;
            offset = (offset + 1) % N;
        }
        self.used += total;
        self.next_seq += 1;
    }
}

impl<const N: usize> Default for KmsgRing<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> Write for KmsgRing<N> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for &b in s.as_bytes() {
            if b == b'\n' {
                self.end_line();
            } else if self.line_len < LINE_MAX {
                self.line[self.line_len] = b;
                self.line_len += 1;
            }
        }
        Ok(())
    }
}

/// Writes into a slice and drops what does not fit.
struct Sink<'a> {
    out: &'a mut [u8],
    len: usize,
}

impl Sink<'_> {
    fn push(&mut self, b: u8) {
        if let Some(slot) = self.out.get_mut(self.len) {
            *slot = b;
            self.len += 1;
        }
    }
}

impl Write for Sink<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        s.bytes().for_each(|b| self.push(b));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::{format, string::String, vec, vec::Vec};

    use super::*;

    /// Reads everything from `cursor` on, as text, one entry per record; an overrun is written as "overrun".
    fn drain<const N: usize>(ring: &KmsgRing<N>, mut cursor: Cursor) -> (Vec<String>, Cursor) {
        let mut out = vec![0; RECORD_MAX];
        let mut lines = Vec::new();
        loop {
            match ring.read(cursor, &mut out) {
                Read::Empty => return (lines, cursor),
                Read::Overrun { oldest } => {
                    lines.push(String::from("overrun"));
                    cursor = oldest;
                }
                Read::Record { len, next } => {
                    lines.push(String::from_utf8(out[..len].to_vec()).unwrap());
                    cursor = next;
                }
            }
        }
    }

    #[test]
    fn a_record_reads_back_in_the_linux_format() {
        let mut ring = KmsgRing::<4096>::new();
        ring.record(3, 1_500_000, "usb: ", format_args!("xHCI {}", "running"));
        let (lines, _) = drain(&ring, ring.start());
        assert_eq!(lines, ["3,0,1500000,-;usb: xHCI running\n"]);
    }

    #[test]
    fn printed_pieces_become_one_line_and_a_record_closes_an_open_line() {
        let mut ring = KmsgRing::<4096>::new();
        ring.text(1, format_args!("hello "));
        ring.text(2, format_args!("world\nsecond"));
        ring.record(4, 3, "", format_args!("warned"));
        let (lines, _) = drain(&ring, ring.start());
        assert_eq!(
            lines,
            [
                "6,0,2,-;hello world\n",
                "6,1,2,-;second\n",
                "4,2,3,-;warned\n"
            ]
        );
    }

    #[test]
    fn a_record_with_newlines_is_split_and_empty_lines_are_dropped() {
        let mut ring = KmsgRing::<4096>::new();
        ring.record(6, 0, "p: ", format_args!("one\n\ntwo\n"));
        let (lines, _) = drain(&ring, ring.start());
        assert_eq!(lines, ["6,0,0,-;p: one\n", "6,1,0,-;two\n"]);
    }

    #[test]
    fn control_bytes_and_the_backslash_are_escaped_as_linux_does() {
        let mut ring = KmsgRing::<4096>::new();
        ring.record(6, 0, "", format_args!("\x1b[31mred\\é"));
        let (lines, _) = drain(&ring, ring.start());
        assert_eq!(lines, ["6,0,0,-;\\x1b[31mred\\x5c\\xc3\\xa9\n"]);
    }

    #[test]
    fn a_long_line_is_cut_at_line_max_and_the_next_line_survives() {
        let mut ring = KmsgRing::<8192>::new();
        let long = "x".repeat(LINE_MAX + 100);
        ring.text(0, format_args!("{}\nnext\n", long));
        let (lines, _) = drain(&ring, ring.start());
        assert_eq!(lines[0].len(), "6,0,0,-;".len() + LINE_MAX + 1);
        assert_eq!(lines[1], "6,1,0,-;next\n");
    }

    #[test]
    fn the_worst_record_fits_record_max() {
        let mut ring = KmsgRing::<8192>::new();
        let long = "\u{1}".repeat(LINE_MAX);
        ring.record(7, u64::MAX, "", format_args!("{}", long));
        let mut out = vec![0; RECORD_MAX];
        let Read::Record { len, .. } = ring.read(ring.start(), &mut out) else {
            panic!("no record");
        };
        assert!(out[..len].ends_with(b"\n"));
        assert_eq!(len, "7,0,18446744073709551615,-;".len() + 4 * LINE_MAX + 1);
    }

    #[test]
    fn old_records_are_overwritten_and_a_slow_reader_is_told() {
        let mut ring = KmsgRing::<256>::new();
        let slow = ring.start();
        for i in 0..100 {
            ring.record(6, i, "", format_args!("record number {}", i));
        }
        let (lines, _) = drain(&ring, slow);
        assert_eq!(lines[0], "overrun");
        // The newest records survive, in order and across the wrap.
        assert!(lines.len() > 3);
        assert_eq!(lines.last().unwrap(), "6,99,99,-;record number 99\n");
        for pair in lines[1..].windows(2) {
            let seq = |l: &str| l.split(',').nth(1).unwrap().parse::<u64>().unwrap();
            assert_eq!(seq(&pair[0]) + 1, seq(&pair[1]));
        }
    }

    #[test]
    fn a_reader_keeping_up_sees_every_record_across_many_wraps() {
        let mut ring = KmsgRing::<512>::new();
        let mut cursor = ring.start();
        for i in 0..5000u64 {
            ring.record(
                6,
                i,
                "t: ",
                format_args!("{}", "y".repeat((i % 40) as usize)),
            );
            let (lines, next) = drain(&ring, cursor);
            assert_eq!(lines.len(), 1, "record {}", i);
            let want = format!("6,{},{},-;t: {}\n", i, i, "y".repeat((i % 40) as usize));
            assert_eq!(lines[0], want);
            cursor = next;
        }
    }

    #[test]
    fn a_line_longer_than_the_ring_is_dropped_without_harm() {
        let mut ring = KmsgRing::<64>::new();
        ring.record(6, 0, "", format_args!("{}", "z".repeat(100)));
        ring.record(6, 0, "", format_args!("small"));
        let (lines, _) = drain(&ring, ring.start());
        assert_eq!(lines, ["6,0,0,-;small\n"]);
    }

    #[test]
    fn a_new_reader_starts_at_the_oldest_record() {
        let mut ring = KmsgRing::<4096>::new();
        ring.record(6, 0, "", format_args!("a"));
        let (_, end) = drain(&ring, ring.start());
        ring.record(6, 0, "", format_args!("b"));
        assert_eq!(drain(&ring, ring.start()).0.len(), 2);
        assert_eq!(drain(&ring, end).0, ["6,1,0,-;b\n"]);
    }
}
