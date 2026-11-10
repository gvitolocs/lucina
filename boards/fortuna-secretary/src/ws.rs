//! The voice socket (SecretaryIOS `docs/lucinavoice-protocol.md` sections 2 and 10): a WebSocket (RFC 6455) to
//! `wss://getsecretary.tech/v1/voice/ws` that takes a voice note's IMA-ADPCM blocks and answers with its transcript,
//! its place in the chat and Lucina's reply, pushed as they happen. Without I/O: the upgrade request and its answer,
//! frames both ways (every frame Fortuna sends is masked, the server's never are), and the protocol's messages.
//! The framing's integer rules are `rules::ws_*` (Verus).

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use serde_json::{json, Value};
use sha1::{Digest, Sha1};

use crate::api::b64_encode;
use crate::rules;

pub const PATH: &str = "/v1/voice/ws";
const GUID: &[u8] = b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

pub const OP_TEXT: u8 = 1;
pub const OP_BINARY: u8 = 2;
pub const OP_CLOSE: u8 = 8;
pub const OP_PING: u8 = 9;
pub const OP_PONG: u8 = 10;

/// The most audio a binary frame carries (the protocol takes up to 8 KiB).
pub const AUDIO_FRAME: usize = 4096;

/// `Sec-WebSocket-Accept` for a key: base64 of SHA-1 over the key and the RFC's GUID.
pub fn accept_for(key: &str) -> String {
    let mut h = Sha1::new();
    h.update(key.as_bytes());
    h.update(GUID);
    b64_encode(&h.finalize())
}

/// The upgrade request. `key`: 16 random bytes, base64.
pub fn upgrade_head(out: &mut Vec<u8>, host: &str, token: &str, key: &str, version: &str) {
    let mut put = |s: &str| out.extend_from_slice(s.as_bytes());
    put("GET ");
    put(PATH);
    put(" HTTP/1.1\r\nHost: ");
    put(host);
    put("\r\nUser-Agent: Fortuna/");
    put(version);
    put(" (Nintendo DSi)\r\nX-Secretary-Client: ");
    put(crate::http::CLIENT_CHANNEL);
    put("\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: ");
    put(key);
    put("\r\nSec-WebSocket-Version: 13\r\nAuthorization: Bearer ");
    put(token);
    put("\r\n\r\n");
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Upgrade {
    /// The answer's head has not all come yet.
    Pending,
    /// Switched: frames start at this offset of the bytes read.
    Open { frames_at: usize },
    /// Another status (401: the token; 404: an old server).
    Refused(u16),
    /// Not an HTTP answer, a head too long, or a wrong `Sec-WebSocket-Accept`.
    Bad,
}

/// Reads the upgrade's answer from the bytes read so far.
pub fn upgrade_answer(buf: &[u8], key: &str) -> Upgrade {
    let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
        return if buf.len() > 4096 { Upgrade::Bad } else { Upgrade::Pending };
    };
    let Ok(head) = core::str::from_utf8(&buf[..end]) else { return Upgrade::Bad };
    let mut lines = head.split("\r\n");
    let status = lines.next().and_then(|l| l.strip_prefix("HTTP/1.1 ")).and_then(|r| r.get(..3)).and_then(|c| c.parse::<u16>().ok());
    let Some(status) = status else { return Upgrade::Bad };
    if status != 101 {
        return Upgrade::Refused(status);
    }
    let (mut upgrade, mut accept) = (false, false);
    let want = accept_for(key);
    for l in lines {
        let Some((k, v)) = l.split_once(':') else { continue };
        let (k, v) = (k.trim(), v.trim());
        if k.eq_ignore_ascii_case("upgrade") && v.eq_ignore_ascii_case("websocket") {
            upgrade = true;
        } else if k.eq_ignore_ascii_case("sec-websocket-accept") && v == want {
            accept = true;
        }
    }
    if upgrade && accept {
        Upgrade::Open { frames_at: end + 4 }
    } else {
        Upgrade::Bad
    }
}

/// One frame from Fortuna: final, `op`, masked with `mask`.
pub fn frame(out: &mut Vec<u8>, op: u8, payload: &[u8], mask: [u8; 4]) {
    let n = payload.len() as u64;
    out.reserve(rules::ws_head_len(n, true) as usize + payload.len());
    out.push(0x80 | (op & 0x0f));
    if n < 126 {
        out.push(0x80 | n as u8);
    } else if n <= 65535 {
        out.push(0x80 | 126);
        out.extend_from_slice(&(n as u16).to_be_bytes());
    } else {
        out.push(0x80 | 127);
        out.extend_from_slice(&n.to_be_bytes());
    }
    out.extend_from_slice(&mask);
    out.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i & 3]));
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub op: u8,
    pub payload: Vec<u8>,
}

/// The server broke the framing rules (`rules::ws_frame_ok`): the socket is closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Broken;

/// Frames from the server, from bytes as they arrive.
#[derive(Default)]
pub struct Decoder {
    buf: Vec<u8>,
}

impl Decoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// The next whole frame, `None` until one has all come.
    pub fn next(&mut self) -> Result<Option<Frame>, Broken> {
        let b = &self.buf;
        if b.len() < 2 {
            return Ok(None);
        }
        let (fin, rsv, op) = (b[0] & 0x80 != 0, (b[0] >> 4) & 7, b[0] & 0x0f);
        let (masked, l7) = (b[1] & 0x80 != 0, b[1] & 0x7f);
        let ext = rules::ws_ext_bytes(l7) as usize;
        if b.len() < 2 + ext {
            return Ok(None);
        }
        let len = match ext {
            0 => l7 as u64,
            2 => u16::from_be_bytes([b[2], b[3]]) as u64,
            _ => u64::from_be_bytes([b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9]]),
        };
        if !rules::ws_frame_ok(fin, rsv, masked, op, len) {
            return Err(Broken);
        }
        let start = 2 + ext;
        let end = start + len as usize;
        if b.len() < end {
            return Ok(None);
        }
        let payload = b[start..end].to_vec();
        self.buf.drain(..end);
        Ok(Some(Frame { op, payload }))
    }
}

// ---- section 10: a voice note's turn ----

/// `start` for a voice note in the owner's main chat: IMA-ADPCM blocks of `block` bytes at `rate` Hz. No `lang`: the
/// server's speech-to-text finds the language (the owner speaks Italian and English).
pub fn note_start(session: &str, rate: u32, block: u32) -> String {
    json!({
        "type": "start", "mode": "ptt", "purpose": "note", "codec": "ima-adpcm", "rate": rate, "block": block,
        "thread": "main", "channel": "web", "source": crate::api::SHARE_SOURCE, "sessionId": session,
    })
    .to_string()
}

pub const END: &str = r#"{"type":"end"}"#;
pub const CANCEL: &str = r#"{"type":"cancel"}"#;

/// What the server says about a note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Down {
    /// The turn began. `codec`: the server knows section 10 (an old one answers without it: use the share route).
    Listening { codec: bool, turn: Option<String> },
    /// The words (never shown on the DSi: a voice card is the note alone); for the timings.
    Transcript,
    /// The note is in the chat.
    Note { share_id: String, user_item_id: Option<String> },
    /// Lucina's answer.
    Reply { share_id: String, text: String, item_id: Option<String> },
    /// The note's turn is over; the server's own times in ms from the release.
    Done { share_id: String, total_ms: Option<u32> },
    Error { code: String, share_id: Option<String> },
    Other,
}

fn s(v: &Value) -> Option<String> {
    v.as_str().map(String::from)
}

pub fn parse_down(text: &[u8]) -> Down {
    let Ok(v) = serde_json::from_slice::<Value>(text) else { return Down::Other };
    let share = || s(&v["shareId"]).unwrap_or_default();
    match v["type"].as_str() {
        Some("listening") => Down::Listening { codec: v.get("codec").is_some_and(|c| !c.is_null()), turn: s(&v["turn"]) },
        Some("transcript") => Down::Transcript,
        Some("note") => Down::Note { share_id: share(), user_item_id: s(&v["userItemId"]) },
        Some("reply") => Down::Reply { share_id: share(), text: s(&v["text"]).unwrap_or_default(), item_id: s(&v["itemId"]) },
        Some("done") => Down::Done { share_id: share(), total_ms: v["ms"]["total"].as_u64().map(|t| t.min(u32::MAX as u64) as u32) },
        Some("error") => Down::Error { code: s(&v["code"]).unwrap_or_default(), share_id: s(&v["shareId"]) },
        _ => Down::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rfcs_accept_key() {
        // RFC 6455 section 1.3
        assert_eq!(accept_for("dGhlIHNhbXBsZSBub25jZQ=="), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }

    #[test]
    fn the_upgrade_and_its_answer() {
        let mut h = Vec::new();
        upgrade_head(&mut h, "getsecretary.tech", "sec_x", "dGhlIHNhbXBsZSBub25jZQ==", "1.62");
        let t = core::str::from_utf8(&h).unwrap();
        assert!(t.starts_with("GET /v1/voice/ws HTTP/1.1\r\nHost: getsecretary.tech\r\n"));
        for want in ["Upgrade: websocket\r\n", "Connection: Upgrade\r\n", "Sec-WebSocket-Version: 13\r\n", "Authorization: Bearer sec_x\r\n", "X-Secretary-Client: web\r\n"] {
            assert!(t.contains(want), "{want}");
        }
        assert!(t.ends_with("\r\n\r\n"));
        let key = "dGhlIHNhbXBsZSBub25jZQ==";
        let ok = b"HTTP/1.1 101 Switching Protocols\r\nupgrade: websocket\r\nconnection: upgrade\r\nsec-websocket-accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n\r\n\x81\x02hi";
        assert_eq!(upgrade_answer(&ok[..20], key), Upgrade::Pending);
        let at = ok.len() - 4;
        assert_eq!(upgrade_answer(ok, key), Upgrade::Open { frames_at: at });
        let wrong = b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nSec-WebSocket-Accept: AAAA\r\n\r\n";
        assert_eq!(upgrade_answer(wrong, key), Upgrade::Bad);
        assert_eq!(upgrade_answer(b"HTTP/1.1 401 Unauthorized\r\n\r\n", key), Upgrade::Refused(401));
        assert_eq!(upgrade_answer(b"garbage\r\n\r\n", key), Upgrade::Bad);
        assert_eq!(upgrade_answer(&[b'x'; 5000], key), Upgrade::Bad);
    }

    /// A server frame (unmasked), any length.
    fn server(op: u8, payload: &[u8]) -> Vec<u8> {
        let mut out = std::vec![0x80 | op];
        let n = payload.len();
        if n < 126 {
            out.push(n as u8);
        } else if n <= 65535 {
            out.push(126);
            out.extend_from_slice(&(n as u16).to_be_bytes());
        } else {
            out.push(127);
            out.extend_from_slice(&(n as u64).to_be_bytes());
        }
        out.extend_from_slice(payload);
        out
    }

    #[test]
    fn frames_come_whole_from_any_cut() {
        let big: Vec<u8> = (0..70_000u32).map(|i| i as u8).collect();
        for n in [0usize, 1, 125, 126, 127, 65535, 65536] {
            let p = &big[..n];
            let bytes = server(OP_BINARY, p);
            let mut d = Decoder::new();
            for chunk in bytes.chunks(997) {
                assert_eq!(d.next(), Ok(None));
                d.feed(chunk);
            }
            assert_eq!(d.next(), Ok(Some(Frame { op: OP_BINARY, payload: p.to_vec() })), "{n} bytes");
            assert_eq!(d.next(), Ok(None));
        }
        // two frames in one read
        let mut d = Decoder::new();
        let mut both = server(OP_TEXT, b"a");
        both.extend(server(OP_PING, b"p"));
        d.feed(&both);
        assert_eq!(d.next().unwrap().unwrap().payload, b"a");
        assert_eq!(d.next().unwrap().unwrap().op, OP_PING);
    }

    #[test]
    fn broken_server_frames_close_the_socket() {
        let big = std::vec![0u8; 65537];
        let mut cases: Vec<Vec<u8>> = std::vec![
            server(OP_BINARY, &big),                 // over the cap
            server(OP_PING, &[0u8; 126]),            // a long control frame
            server(3, b"x"),                         // an unknown opcode
            server(0, b"x"),                         // a continuation
        ];
        let mut masked = server(OP_TEXT, b"x");
        masked[1] |= 0x80;
        cases.push(masked);
        let mut not_final = server(OP_TEXT, b"x");
        not_final[0] &= 0x7f;
        cases.push(not_final);
        let mut rsv = server(OP_TEXT, b"x");
        rsv[0] |= 0x40;
        cases.push(rsv);
        for (i, c) in cases.iter().enumerate() {
            let mut d = Decoder::new();
            d.feed(c);
            assert_eq!(d.next(), Err(Broken), "case {i}");
        }
    }

    #[test]
    fn fortunas_frames_are_masked() {
        for n in [0usize, 5, 125, 126, 4096, 65536] {
            let p: Vec<u8> = (0..n).map(|i| (i * 7) as u8).collect();
            let mut f = Vec::new();
            frame(&mut f, OP_BINARY, &p, [1, 2, 3, 4]);
            assert_eq!(f[0], 0x82);
            assert_eq!(f[1] & 0x80, 0x80, "mask bit");
            let head = rules::ws_head_len(n as u64, true) as usize;
            assert_eq!(f.len(), head + n);
            let mask = &f[head - 4..head];
            let back: Vec<u8> = f[head..].iter().enumerate().map(|(i, b)| b ^ mask[i & 3]).collect();
            assert_eq!(back, p);
        }
    }

    #[test]
    fn note_messages() {
        let st: Value = serde_json::from_str(&note_start("fortuna-dev_1", 10909, 256)).unwrap();
        assert_eq!(st["purpose"], "note");
        assert_eq!(st["codec"], "ima-adpcm");
        assert_eq!((st["rate"].as_u64(), st["block"].as_u64()), (Some(10909), Some(256)));
        assert_eq!((st["thread"].as_str(), st["source"].as_str(), st["sessionId"].as_str()), (Some("main"), Some("fortuna-chat"), Some("fortuna-dev_1")));
        assert!(st.get("lang").is_none());
        assert_eq!(parse_down(br#"{"type":"listening","purpose":"note","codec":"ima-adpcm","rate":10909,"turn":"t1"}"#), Down::Listening { codec: true, turn: Some("t1".into()) });
        assert_eq!(parse_down(br#"{"type":"listening"}"#), Down::Listening { codec: false, turn: None });
        assert_eq!(parse_down(br#"{"type":"note","shareId":"t1","userItemId":"u1"}"#), Down::Note { share_id: "t1".into(), user_item_id: Some("u1".into()) });
        assert_eq!(
            parse_down(br#"{"type":"reply","shareId":"t1","text":"Ciao","itemId":"l1"}"#),
            Down::Reply { share_id: "t1".into(), text: "Ciao".into(), item_id: Some("l1".into()) }
        );
        assert_eq!(parse_down(br#"{"type":"done","shareId":"t1","ms":{"stt":80,"total":2400}}"#), Down::Done { share_id: "t1".into(), total_ms: Some(2400) });
        assert_eq!(parse_down(br#"{"type":"error","code":"no_speech","message":"x"}"#), Down::Error { code: "no_speech".into(), share_id: None });
        assert_eq!(parse_down(br#"{"type":"transcript","text":"x"}"#), Down::Transcript);
        assert_eq!(parse_down(b"nope"), Down::Other);
    }
}
