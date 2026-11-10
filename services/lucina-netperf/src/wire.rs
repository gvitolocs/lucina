//! What the two ends say to each other, and the sum smoltcp computes for every packet.

use std::io::{self, Read, Write};

/// How much one `read` or `write` asks for.
pub const CHUNK: usize = 64 * 1024;
/// A UDP payload that fits a 1500-byte MTU without fragments.
pub const DATAGRAM: usize = 1472;

const MAGIC: [u8; 4] = *b"LNP1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    /// From the client to the server.
    Up,
    /// From the server to the client.
    Down,
}

impl Dir {
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "up" => Some(Dir::Up),
            "down" => Some(Dir::Down),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Dir::Up => "up",
            Dir::Down => "down",
        }
    }
}

/// The first thing a TCP client sends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hello {
    Tcp(Dir, u32),
    /// As `Tcp`, with data the receiver can check byte by byte: [`Pattern`].
    Check(Dir, u32),
    Echo,
}

impl Hello {
    pub fn write(self, out: &mut impl Write) -> io::Result<()> {
        let (kind, secs) = match self {
            Hello::Tcp(Dir::Up, secs) => (0u8, secs),
            Hello::Tcp(Dir::Down, secs) => (1, secs),
            Hello::Echo => (2, 0),
            Hello::Check(Dir::Up, secs) => (3, secs),
            Hello::Check(Dir::Down, secs) => (4, secs),
        };
        let mut bytes = [0u8; 9];
        bytes[..4].copy_from_slice(&MAGIC);
        bytes[4] = kind;
        bytes[5..].copy_from_slice(&secs.to_le_bytes());
        out.write_all(&bytes)
    }

    pub fn read(input: &mut impl Read) -> io::Result<Self> {
        let mut bytes = [0u8; 9];
        input.read_exact(&mut bytes)?;
        if bytes[..4] != MAGIC {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let mut secs = [0u8; 4];
        secs.copy_from_slice(&bytes[5..]);
        let secs = u32::from_le_bytes(secs);
        match bytes[4] {
            0 => Ok(Hello::Tcp(Dir::Up, secs)),
            1 => Ok(Hello::Tcp(Dir::Down, secs)),
            2 => Ok(Hello::Echo),
            3 => Ok(Hello::Check(Dir::Up, secs)),
            4 => Ok(Hello::Check(Dir::Down, secs)),
            _ => Err(io::ErrorKind::InvalidData.into()),
        }
    }
}

/// A stream of bytes that depend on where they are: a lost, stale or moved byte shows, which a speed test would not see.
#[derive(Clone, Copy, Debug, Default)]
pub struct Pattern {
    offset: u64,
}

impl Pattern {
    fn next_byte(&mut self) -> u8 {
        let word = (self.offset / 8 + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        let byte = word.to_le_bytes()[(self.offset % 8) as usize];
        self.offset += 1;
        byte
    }

    pub fn fill(&mut self, out: &mut [u8]) {
        for byte in out {
            *byte = self.next_byte();
        }
    }

    /// How many bytes differ from what should be at this point in the stream.
    pub fn wrong_bytes(&mut self, data: &[u8]) -> u64 {
        data.iter()
            .map(|byte| u64::from(*byte != self.next_byte()))
            .sum()
    }
}

/// A short datagram with the magic is a request; anything else is data to count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UdpHello {
    SendToMe { secs: u32, mbit: u32 },
    Report,
}

impl UdpHello {
    pub fn encode(self) -> [u8; 13] {
        let mut bytes = [0u8; 13];
        bytes[..4].copy_from_slice(&MAGIC);
        match self {
            UdpHello::SendToMe { secs, mbit } => {
                bytes[4] = 1;
                bytes[5..9].copy_from_slice(&secs.to_le_bytes());
                bytes[9..].copy_from_slice(&mbit.to_le_bytes());
            }
            UdpHello::Report => bytes[4] = 2,
        }
        bytes
    }

    pub fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != 13 || bytes[..4] != MAGIC {
            return None;
        }
        let field = |at: usize| {
            let mut value = [0u8; 4];
            value.copy_from_slice(&bytes[at..at + 4]);
            u32::from_le_bytes(value)
        };
        match bytes[4] {
            1 => Some(UdpHello::SendToMe {
                secs: field(5),
                mbit: field(9),
            }),
            2 => Some(UdpHello::Report),
            _ => None,
        }
    }
}

pub fn udp_sequence(sequence: u64) -> [u8; 8] {
    sequence.to_le_bytes()
}

/// RFC 1071, one 16-bit word at a time, as `smoltcp::wire::ip::checksum::data` does.
pub fn internet_checksum(data: &[u8]) -> u16 {
    let mut sum = 0u32;
    let mut words = data.chunks_exact(2);
    for word in &mut words {
        sum += u32::from(u16::from_be_bytes([word[0], word[1]]));
        if sum > 0xffff {
            sum -= 0xffff;
        }
    }
    if let [last] = words.remainder() {
        sum += u32::from(*last) << 8;
    }
    while sum > 0xffff {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hellos_survive_the_wire() {
        for hello in [
            Hello::Tcp(Dir::Up, 7),
            Hello::Tcp(Dir::Down, 30),
            Hello::Check(Dir::Up, 9),
            Hello::Check(Dir::Down, 11),
            Hello::Echo,
        ] {
            let mut bytes = Vec::new();
            hello.write(&mut bytes).unwrap();
            assert_eq!(Hello::read(&mut bytes.as_slice()).unwrap(), hello);
        }
        for hello in [UdpHello::SendToMe { secs: 5, mbit: 90 }, UdpHello::Report] {
            assert_eq!(UdpHello::parse(&hello.encode()), Some(hello));
        }
    }

    #[test]
    fn the_pattern_finds_a_changed_byte_wherever_the_reads_fall() {
        let mut sent = vec![0u8; 100_000];
        Pattern::default().fill(&mut sent);
        let mut receiver = Pattern::default();
        let clean: u64 = sent.chunks(1337).map(|part| receiver.wrong_bytes(part)).sum();
        assert_eq!(clean, 0);

        sent[54_321] ^= 1;
        let mut receiver = Pattern::default();
        let one: u64 = sent.chunks(1337).map(|part| receiver.wrong_bytes(part)).sum();
        assert_eq!(one, 1);

        // A whole frame from elsewhere in the stream shows too, as a stale cache line would.
        sent[54_321] ^= 1;
        let stale = sent[0..1460].to_vec();
        sent[20_000..21_460].copy_from_slice(&stale);
        let mut receiver = Pattern::default();
        let frame: u64 = sent.chunks(1337).map(|part| receiver.wrong_bytes(part)).sum();
        assert!(frame > 1000, "{frame}");
    }

    #[test]
    fn a_stranger_is_not_a_hello() {
        assert!(Hello::read(&mut &b"GET / HTTP"[..]).is_err());
        assert_eq!(UdpHello::parse(&[0x55; DATAGRAM]), None);
        assert_eq!(UdpHello::parse(&[0x55; 13]), None);
    }

    /// The worked example of RFC 1071, section 3.
    #[test]
    fn checksum_of_the_rfc_example() {
        let data = [0x00, 0x01, 0xf2, 0x03, 0xf4, 0xf5, 0xf6, 0xf7];
        assert_eq!(internet_checksum(&data), !0xddf2);
    }
}
