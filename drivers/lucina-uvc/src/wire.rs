//! What `/dev/lucina-camera0` speaks, for the kernel and for user space (`lucina-camera`).
//!
//! - A write is one command line: `start <FOURCC> <width> <height> <interval>` (interval in 100 ns units, as UVC counts it:
//!   333333 is 30 frames a second) or `stop`. Closing the file stops the stream too.
//! - A read gives the newest whole frame the reader has not had yet: a [`Header`] of [`HEADER_LEN`] bytes, then the frame's
//!   bytes (YUY2 as the camera sends it, or one JPEG). A read blocks until there is one (`EAGAIN` with `O_NONBLOCK`); `poll`
//!   says IN when there is one. A buffer too small for the header and the frame gets `EINVAL`, and that frame is skipped
//!   (the next one comes a frame interval later).
//! - Only one file may be open at a time (`EBUSY`): one app owns the camera while it has it open.
//!
//! `/proc/lucina-camera` says what the camera offers and what it does, as `key: value` lines.

use alloc::string::String;
use core::fmt::Write as _;

pub const HEADER_LEN: usize = 32;
pub const MAGIC: [u8; 4] = *b"LCAM";
pub const VERSION: u16 = 1;
/// [`Header::flags`]: the frame has Linux's error flag (a lost packet, an overflow, the camera's error bit).
pub const FLAG_ERROR: u16 = 1 << 0;

/// The frame header, little-endian.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub flags: u16,
    pub fourcc: [u8; 4],
    pub width: u16,
    pub height: u16,
    /// The frame's length after the header.
    pub bytes: u32,
    /// [`crate::stream::Completed::sequence`].
    pub sequence: u32,
    /// When the frame was complete, on the monotonic clock (`CLOCK_MONOTONIC`), in microseconds.
    pub time_us: u64,
}

impl Header {
    pub fn encode(&self) -> [u8; HEADER_LEN] {
        let mut d = [0u8; HEADER_LEN];
        d[0..4].copy_from_slice(&MAGIC);
        d[4..6].copy_from_slice(&VERSION.to_le_bytes());
        d[6..8].copy_from_slice(&self.flags.to_le_bytes());
        d[8..12].copy_from_slice(&self.fourcc);
        d[12..14].copy_from_slice(&self.width.to_le_bytes());
        d[14..16].copy_from_slice(&self.height.to_le_bytes());
        d[16..20].copy_from_slice(&self.bytes.to_le_bytes());
        d[20..24].copy_from_slice(&self.sequence.to_le_bytes());
        d[24..32].copy_from_slice(&self.time_us.to_le_bytes());
        d
    }

    pub fn decode(d: &[u8]) -> Option<Header> {
        if d.len() < HEADER_LEN || d[0..4] != MAGIC || u16::from_le_bytes([d[4], d[5]]) != VERSION {
            return None;
        }
        let u16_at = |i: usize| u16::from_le_bytes([d[i], d[i + 1]]);
        let u32_at = |i: usize| u32::from_le_bytes([d[i], d[i + 1], d[i + 2], d[i + 3]]);
        let mut time = [0u8; 8];
        time.copy_from_slice(&d[24..32]);
        Some(Header {
            flags: u16_at(6),
            fourcc: [d[8], d[9], d[10], d[11]],
            width: u16_at(12),
            height: u16_at(14),
            bytes: u32_at(16),
            sequence: u32_at(20),
            time_us: u64::from_le_bytes(time),
        })
    }

    pub fn error(&self) -> bool {
        self.flags & FLAG_ERROR != 0
    }
}

/// A stream request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamRequest {
    pub fourcc: [u8; 4],
    pub width: u16,
    pub height: u16,
    pub interval: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Start(StreamRequest),
    Stop,
}

impl Command {
    pub fn parse(line: &str) -> Option<Command> {
        let mut words = line.split_ascii_whitespace();
        let command = match words.next()? {
            "stop" => Command::Stop,
            "start" => {
                let fourcc: [u8; 4] = words.next()?.as_bytes().try_into().ok()?;
                let width = words.next()?.parse().ok()?;
                let height = words.next()?.parse().ok()?;
                let interval = words.next()?.parse().ok()?;
                Command::Start(StreamRequest { fourcc, width, height, interval })
            }
            _ => return None,
        };
        if words.next().is_some() {
            return None;
        }
        Some(command)
    }

    pub fn line(&self) -> String {
        match self {
            Command::Stop => String::from("stop\n"),
            Command::Start(r) => {
                let mut s = String::new();
                let _ = writeln!(
                    s,
                    "start {} {} {} {}",
                    core::str::from_utf8(&r.fourcc).unwrap_or("????"),
                    r.width,
                    r.height,
                    r.interval
                );
                s
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_round_trip() {
        let h = Header { flags: FLAG_ERROR, fourcc: *b"YUY2", width: 640, height: 480, bytes: 614400, sequence: 7, time_us: 1 << 40 };
        assert_eq!(Header::decode(&h.encode()), Some(h));
        let mut bad = h.encode();
        bad[0] = b'X';
        assert_eq!(Header::decode(&bad), None);
    }

    #[test]
    fn commands() {
        let start = Command::Start(StreamRequest { fourcc: *b"MJPG", width: 1280, height: 720, interval: 333333 });
        assert_eq!(Command::parse(&start.line()), Some(start));
        assert_eq!(Command::parse("stop\n"), Some(Command::Stop));
        assert_eq!(Command::parse("start MJPG 1280 720"), None);
        assert_eq!(Command::parse("start MJPEG 1280 720 1"), None);
        assert_eq!(Command::parse("stop now"), None);
    }
}
