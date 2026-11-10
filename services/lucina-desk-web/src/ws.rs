//! The server side of RFC 6455, by hand rather than with `tungstenite`: the host needs only a server that reads masked frames
//! up to a cap and writes unmasked ones, on blocking sockets. `tungstenite` would bring `rand` (with its own getrandom), `http`
//! and `bytes` into a static program that runs on Asterinas' Linux ABI, for a client half that is never used and a read path
//! whose message cap we would set anyway. The tests talk to this server with `tungstenite`'s client, so the two implementations
//! check each other.

use std::io::{self, Read, Write};

const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// What a client sent, whole (fragments joined).
#[derive(Debug, PartialEq, Eq)]
pub enum Message {
    Text(String),
    Binary(Vec<u8>),
    Ping(Vec<u8>),
    Pong,
    Close,
}

/// `Sec-WebSocket-Accept` for a client's `Sec-WebSocket-Key`.
pub fn accept_key(key: &str) -> String {
    let mut h = sha1_smol::Sha1::new();
    h.update(key.trim().as_bytes());
    h.update(GUID.as_bytes());
    base64(&h.digest().bytes())
}

fn base64(b: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(b.len().div_ceil(3) * 4);
    for c in b.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                s.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

fn bad(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why.to_owned())
}

/// Reads a client's messages. Holds a fragmented message across the control frames that may come between its parts.
#[derive(Default)]
pub struct Reader {
    partial: Option<(u8, Vec<u8>)>,
}

impl Reader {
    /// The next whole message. Control frames come back as they arrive, also between the fragments of a message. A message
    /// longer than `max` bytes, an unmasked frame (only a server may send those) or an unknown opcode is an error, and the
    /// caller drops the connection.
    pub fn read(&mut self, r: &mut impl Read, max: usize) -> io::Result<Message> {
        read_message(r, max, &mut self.partial)
    }
}

fn read_message(r: &mut impl Read, max: usize, partial: &mut Option<(u8, Vec<u8>)>) -> io::Result<Message> {
    loop {
        let mut h = [0u8; 2];
        r.read_exact(&mut h)?;
        let fin = h[0] & 0x80 != 0;
        let op = h[0] & 0x0f;
        if h[1] & 0x80 == 0 {
            return Err(bad("an unmasked frame from a client"));
        }
        let len = match h[1] & 0x7f {
            126 => {
                let mut b = [0u8; 2];
                r.read_exact(&mut b)?;
                u16::from_be_bytes(b) as u64
            }
            127 => {
                let mut b = [0u8; 8];
                r.read_exact(&mut b)?;
                u64::from_be_bytes(b)
            }
            n => n as u64,
        };
        let so_far = partial.as_ref().map_or(0, |p| p.1.len()) as u64;
        if op < 8 && so_far + len > max as u64 || op >= 8 && len > 125 {
            return Err(bad("a message over the size limit"));
        }
        let mut mask = [0u8; 4];
        r.read_exact(&mut mask)?;
        let mut data = vec![0u8; len as usize];
        r.read_exact(&mut data)?;
        for (i, b) in data.iter_mut().enumerate() {
            *b ^= mask[i & 3];
        }
        match op {
            0x8 => return Ok(Message::Close),
            0x9 => return Ok(Message::Ping(data)),
            0xa => return Ok(Message::Pong),
            0x0 => {
                let Some((first, mut got)) = partial.take() else { return Err(bad("a continuation without a start")) };
                got.extend_from_slice(&data);
                if !fin {
                    *partial = Some((first, got));
                    continue;
                }
                return whole(first, got);
            }
            0x1 | 0x2 => {
                if partial.is_some() {
                    return Err(bad("a new message inside a fragmented one"));
                }
                if !fin {
                    *partial = Some((op, data));
                    continue;
                }
                return whole(op, data);
            }
            _ => return Err(bad("an unknown opcode")),
        }
    }
}

fn whole(op: u8, data: Vec<u8>) -> io::Result<Message> {
    if op == 0x1 {
        String::from_utf8(data).map(Message::Text).map_err(|_| bad("text that is not UTF-8"))
    } else {
        Ok(Message::Binary(data))
    }
}

pub const TEXT: u8 = 0x1;
pub const BINARY: u8 = 0x2;
pub const CLOSE: u8 = 0x8;
pub const PONG: u8 = 0xa;

/// One unmasked, final frame.
pub fn write_frame(w: &mut impl Write, op: u8, payload: &[u8]) -> io::Result<()> {
    let mut h = Vec::with_capacity(10);
    h.push(0x80 | op);
    match payload.len() {
        n if n < 126 => h.push(n as u8),
        n if n <= 0xffff => {
            h.push(126);
            h.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            h.push(127);
            h.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    w.write_all(&h)?;
    w.write_all(payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_accept_key_of_rfc_6455s_example() {
        assert_eq!(accept_key("dGhlIHNhbXBsZSBub25jZQ=="), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
        assert_eq!(base64(b"ab"), "YWI=");
        assert_eq!(base64(b"a"), "YQ==");
        assert_eq!(base64(b""), "");
    }

    fn masked(fin: bool, op: u8, payload: &[u8]) -> Vec<u8> {
        let mask = [1u8, 2, 3, 4];
        let mut f = vec![(fin as u8) << 7 | op];
        match payload.len() {
            n if n < 126 => f.push(0x80 | n as u8),
            n if n <= 0xffff => {
                f.push(0x80 | 126);
                f.extend_from_slice(&(n as u16).to_be_bytes());
            }
            n => {
                f.push(0x80 | 127);
                f.extend_from_slice(&(n as u64).to_be_bytes());
            }
        }
        f.extend_from_slice(&mask);
        f.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i & 3]));
        f
    }

    #[test]
    fn fragments_join_around_a_ping_and_long_lengths_read() {
        let mut s = masked(false, 1, b"a,");
        s.extend(masked(true, 9, b"hi"));
        s.extend(masked(true, 0, b"7"));
        let big = vec![7u8; 70_000];
        s.extend(masked(true, 2, &big));
        let (mut r, mut m) = (&s[..], Reader::default());
        assert_eq!(m.read(&mut r, 1 << 20).unwrap(), Message::Ping(b"hi".to_vec()));
        assert_eq!(m.read(&mut r, 1 << 20).unwrap(), Message::Text("a,7".into()));
        assert_eq!(m.read(&mut r, 1 << 20).unwrap(), Message::Binary(big));
    }

    #[test]
    fn too_long_unmasked_or_unknown_is_refused() {
        assert!(Reader::default().read(&mut &masked(true, 2, &[0; 200])[..], 100).is_err());
        let mut parts = masked(false, 2, &[0; 60]);
        parts.extend(masked(true, 0, &[0; 60]));
        assert!(Reader::default().read(&mut &parts[..], 100).is_err());
        assert!(Reader::default().read(&mut &[0x82u8, 0x01, 0x00][..], 100).is_err());
        assert!(Reader::default().read(&mut &masked(true, 3, b"")[..], 100).is_err());
        assert!(Reader::default().read(&mut &masked(true, 1, &[0xff])[..], 100).is_err());
    }

    #[test]
    fn frames_written_have_the_right_length_forms() {
        for n in [0usize, 125, 126, 65535, 65536] {
            let mut out = Vec::new();
            write_frame(&mut out, BINARY, &vec![1; n]).unwrap();
            let head = match n {
                0..=125 => 2,
                126..=65535 => 4,
                _ => 10,
            };
            assert_eq!(out.len(), head + n);
            assert_eq!(out[0], 0x82);
        }
    }
}
