// SPDX-License-Identifier: MPL-2.0

//! The stream a reader of `/dev/lucina-gesture` gets: a 32-byte [`Header`] first, then records of [`RECORD_LEN`] bytes.
//!
//! The header is the same size as a record so that a reader works in units of 32 bytes throughout. Its kind byte is 0,
//! which no record has ([`GestureRecord::from_bytes`] refuses kind 0), so a header cannot be taken for a record or the
//! other way round.
//!
//! | Offset | Type | Meaning |
//! | --- | --- | --- |
//! | 0 | 4 bytes | `b"LGST"` |
//! | 4 | `u16` | [`FORMAT_VERSION`] |
//! | 6 | `u16` | the record length, [`RECORD_LEN`] |
//! | 8 | `u8` | 0: the kind byte of a header |
//! | 9 | 23 bytes | 0, reserved |

use crate::{DecodeError, GestureRecord, RECORD_LEN};

/// The record format this crate reads and writes. A reader that finds another refuses the stream rather than guess.
pub const FORMAT_VERSION: u16 = 1;
/// Bytes in the header: one record's worth.
pub const HEADER_LEN: usize = RECORD_LEN;
/// The first four bytes of a header.
pub const MAGIC: [u8; 4] = *b"LGST";

/// The first 32 bytes a reader gets after opening the device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    /// The record format: [`FORMAT_VERSION`] for what this crate writes.
    pub version: u16,
    /// Bytes per record after the header.
    pub record_len: u16,
}

/// Why 32 bytes are no header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeaderError {
    /// Fewer than [`HEADER_LEN`] bytes.
    Short,
    /// The first four bytes are not `LGST`.
    Magic,
    /// Byte 8 is not 0: this is a record, or something else.
    NotHeader,
}

/// Why a stream cannot be read by this crate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mismatch {
    /// No header.
    Header(HeaderError),
    /// A format version this crate does not know.
    Version { got: u16, want: u16 },
    /// A record length that is not this crate's.
    RecordLen { got: u16, want: u16 },
}

impl Header {
    /// What this crate writes.
    pub const CURRENT: Header = Header {
        version: FORMAT_VERSION,
        record_len: RECORD_LEN as u16,
    };

    /// The 32 bytes of the table above.
    pub fn to_bytes(&self) -> [u8; HEADER_LEN] {
        let mut b = [0u8; HEADER_LEN];
        b[0..4].copy_from_slice(&MAGIC);
        b[4..6].copy_from_slice(&self.version.to_le_bytes());
        b[6..8].copy_from_slice(&self.record_len.to_le_bytes());
        b
    }

    /// The header in the first [`HEADER_LEN`] bytes of `b`. Reads the version and the record length as they are; whether
    /// this crate can read them is [`check_header`]'s question.
    pub fn parse(b: &[u8]) -> Result<Header, HeaderError> {
        if b.len() < HEADER_LEN {
            return Err(HeaderError::Short);
        }
        if b[0..4] != MAGIC {
            return Err(HeaderError::Magic);
        }
        if b[8] != 0 {
            return Err(HeaderError::NotHeader);
        }
        Ok(Header {
            version: u16::from_le_bytes([b[4], b[5]]),
            record_len: u16::from_le_bytes([b[6], b[7]]),
        })
    }
}

/// Whether the stream starting at `b` is one this crate reads: a header with [`FORMAT_VERSION`] and [`RECORD_LEN`].
pub fn check_header(b: &[u8]) -> Result<(), Mismatch> {
    let h = Header::parse(b).map_err(Mismatch::Header)?;
    if h.version != FORMAT_VERSION {
        return Err(Mismatch::Version {
            got: h.version,
            want: FORMAT_VERSION,
        });
    }
    if h.record_len as usize != RECORD_LEN {
        return Err(Mismatch::RecordLen {
            got: h.record_len,
            want: RECORD_LEN as u16,
        });
    }
    Ok(())
}

/// The records of a stream (header first), one at a time. A trailing partial record comes out as
/// [`DecodeError::Short`], and then there is nothing more.
pub struct StreamReader<'a> {
    rest: &'a [u8],
}

impl<'a> StreamReader<'a> {
    /// A reader over `b`, which must start with a header this crate reads.
    pub fn new(b: &'a [u8]) -> Result<StreamReader<'a>, Mismatch> {
        check_header(b)?;
        Ok(StreamReader {
            rest: &b[HEADER_LEN..],
        })
    }
}

impl Iterator for StreamReader<'_> {
    type Item = Result<GestureRecord, DecodeError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.rest.is_empty() {
            return None;
        }
        if self.rest.len() < RECORD_LEN {
            self.rest = &[];
            return Some(Err(DecodeError::Short));
        }
        let (one, rest) = self.rest.split_at(RECORD_LEN);
        self.rest = rest;
        Some(GestureRecord::from_bytes(one))
    }
}

/// The header and then `records` into `out`; the bytes written, or None if `out` is too short for them.
pub fn write_stream(records: &[GestureRecord], out: &mut [u8]) -> Option<usize> {
    let need = HEADER_LEN.checked_add(records.len().checked_mul(RECORD_LEN)?)?;
    if out.len() < need {
        return None;
    }
    out[..HEADER_LEN].copy_from_slice(&Header::CURRENT.to_bytes());
    for (i, r) in records.iter().enumerate() {
        let at = HEADER_LEN + i * RECORD_LEN;
        out[at..at + RECORD_LEN].copy_from_slice(&r.to_bytes());
    }
    Some(need)
}

/// Subtracts the first record's `time_us` from every record's (saturating), so a recording starts at 0.
pub fn rebase_to_zero(records: &mut [GestureRecord]) {
    let Some(base) = records.first().map(|r| r.time_us) else {
        return;
    };
    for r in records {
        r.time_us = r.time_us.saturating_sub(base);
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use crate::{Kind, Phase, SCALE_ONE};
    use std::vec;
    use std::vec::Vec;

    fn rec(t: u64, phase: Phase, seq: u32) -> GestureRecord {
        GestureRecord {
            time_us: t,
            kind: Kind::Swipe,
            phase,
            fingers: 3,
            flags: 0,
            dx: 0,
            dy: 0,
            scale: SCALE_ONE,
            angle: 0,
            seq,
        }
    }

    #[test]
    fn the_header_is_the_table() {
        let b = Header::CURRENT.to_bytes();
        let mut want = [0u8; 32];
        want[..8].copy_from_slice(&[b'L', b'G', b'S', b'T', 1, 0, 32, 0]);
        assert_eq!(b, want);
        assert_eq!(Header::parse(&b), Ok(Header::CURRENT));
        assert_eq!(check_header(&b), Ok(()));
    }

    #[test]
    fn header_parse_refuses_what_is_not_one() {
        let ok = Header::CURRENT.to_bytes();
        assert_eq!(Header::parse(&ok[..31]), Err(HeaderError::Short));
        assert_eq!(Header::parse(&[]), Err(HeaderError::Short));
        let mut b = ok;
        b[0] = b'X';
        assert_eq!(Header::parse(&b), Err(HeaderError::Magic));
        let mut b = ok;
        b[8] = 1;
        assert_eq!(Header::parse(&b), Err(HeaderError::NotHeader));
        // a record is not a header, whatever its first bytes
        assert_eq!(
            Header::parse(&rec(0, Phase::Begin, 0).to_bytes()),
            Err(HeaderError::Magic)
        );
    }

    #[test]
    fn check_header_names_the_mismatch() {
        let mut b = Header::CURRENT.to_bytes();
        b[4] = 2;
        assert_eq!(check_header(&b), Err(Mismatch::Version { got: 2, want: 1 }));
        let mut b = Header::CURRENT.to_bytes();
        b[6] = 40;
        assert_eq!(
            check_header(&b),
            Err(Mismatch::RecordLen { got: 40, want: 32 })
        );
        assert_eq!(
            check_header(&b[..5]),
            Err(Mismatch::Header(HeaderError::Short))
        );
    }

    #[test]
    fn write_then_read_round_trips() {
        let rs = [
            rec(5, Phase::Begin, 7),
            rec(9, Phase::Update, 7),
            rec(12, Phase::End, 7),
        ];
        let mut buf = vec![0u8; 32 + 3 * 32];
        assert_eq!(write_stream(&rs, &mut buf), Some(128));
        let back: Vec<_> = StreamReader::new(&buf).unwrap().collect();
        assert_eq!(back, rs.iter().map(|r| Ok(*r)).collect::<Vec<_>>());
        // empty stream: just the header
        let mut buf = [0u8; 32];
        assert_eq!(write_stream(&[], &mut buf), Some(32));
        assert_eq!(StreamReader::new(&buf).unwrap().count(), 0);
    }

    #[test]
    fn write_stream_refuses_a_short_buffer() {
        let rs = [rec(5, Phase::Begin, 7)];
        let mut buf = [0u8; 63];
        assert_eq!(write_stream(&rs, &mut buf), None);
        let mut buf = [0u8; 31];
        assert_eq!(write_stream(&[], &mut buf), None);
    }

    #[test]
    fn a_trailing_partial_record_is_short_and_the_last() {
        let mut buf = vec![0u8; 64];
        write_stream(&[rec(1, Phase::Begin, 0)], &mut buf);
        buf.extend_from_slice(&[0u8; 10]);
        let mut r = StreamReader::new(&buf).unwrap();
        assert!(matches!(r.next(), Some(Ok(_))));
        assert_eq!(r.next(), Some(Err(DecodeError::Short)));
        assert_eq!(r.next(), None);
    }

    #[test]
    fn a_bad_record_is_an_error_and_the_reader_goes_on() {
        let mut buf = vec![0u8; 96];
        write_stream(&[rec(1, Phase::Begin, 0), rec(2, Phase::End, 0)], &mut buf);
        buf[32 + 8] = 9; // the first record's kind
        let got: Vec<_> = StreamReader::new(&buf).unwrap().collect();
        assert_eq!(got[0], Err(DecodeError::Kind(9)));
        assert!(got[1].is_ok());
    }

    #[test]
    fn a_stream_without_a_header_is_refused() {
        let r = rec(1, Phase::Begin, 0).to_bytes();
        assert!(matches!(
            StreamReader::new(&r),
            Err(Mismatch::Header(HeaderError::Magic))
        ));
        assert!(matches!(
            StreamReader::new(&[]),
            Err(Mismatch::Header(HeaderError::Short))
        ));
    }

    #[test]
    fn rebase_subtracts_the_first_time_saturating() {
        let mut rs = [
            rec(1000, Phase::Begin, 0),
            rec(1500, Phase::Update, 0),
            rec(900, Phase::End, 0),
        ];
        rebase_to_zero(&mut rs);
        assert_eq!(rs.map(|r| r.time_us), [0, 500, 0]);
        rebase_to_zero(&mut []);
    }
}
