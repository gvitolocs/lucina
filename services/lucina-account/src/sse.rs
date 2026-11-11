//! An incremental Server-Sent Events parser: bytes in, whole blocks out. The behaviour of `SseParser` in SecretaryIOS
//! `crates/secretary-core/src/chat_stream.rs` (that crate brings reqwest and tokio), plus the `id` field, which
//! `GET /v1/timeline/stream` resumes from.

/// One parsed block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub event: String,
    pub data: String,
    pub id: Option<String>,
}

/// Handles `\n`, `\r\n` and `\r` line ends, comment lines (`: ping`), `event:`, `id:` and multi-line `data:`.
#[derive(Debug, Default)]
pub struct Parser {
    line: Vec<u8>,
    after_cr: bool,
    event: Option<String>,
    data: Vec<String>,
    id: Option<String>,
}

impl Parser {
    pub fn push(&mut self, bytes: &[u8]) -> Vec<Block> {
        let mut out = Vec::new();
        for &b in bytes {
            match b {
                b'\n' if self.after_cr => self.after_cr = false,
                b'\n' | b'\r' => {
                    self.after_cr = b == b'\r';
                    let line = std::mem::take(&mut self.line);
                    self.line_done(&line, &mut out);
                }
                _ => {
                    self.after_cr = false;
                    self.line.push(b);
                }
            }
        }
        out
    }

    fn line_done(&mut self, line: &[u8], out: &mut Vec<Block>) {
        if line.is_empty() {
            if !self.data.is_empty() || self.event.is_some() {
                out.push(Block {
                    event: self.event.take().unwrap_or_else(|| "message".into()),
                    data: std::mem::take(&mut self.data).join("\n"),
                    id: self.id.take(),
                });
            }
            return;
        }
        if line[0] == b':' {
            return;
        }
        let text = String::from_utf8_lossy(line);
        let (field, value) = match text.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (text.as_ref(), ""),
        };
        match field {
            "event" => self.event = Some(value.to_owned()),
            "data" => self.data.push(value.to_owned()),
            "id" => self.id = Some(value.to_owned()),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_block_split_anywhere_comes_out_whole() {
        let all = ": ping\n\nevent: item\nid: tl_1\ndata: {\"id\":\"tl_1\"}\n\n";
        let mut whole = Parser::default();
        let want = whole.push(all.as_bytes());
        assert_eq!(want.len(), 1);
        assert_eq!(want[0].event, "item");
        assert_eq!(want[0].id.as_deref(), Some("tl_1"));
        let mut bytewise = Parser::default();
        let got: Vec<Block> = all.bytes().flat_map(|b| bytewise.push(&[b])).collect();
        assert_eq!(got, want);
    }

    #[test]
    fn crlf_and_multiline_data() {
        let mut p = Parser::default();
        let b = p.push(b"data: a\r\ndata: b\r\n\r\nevent: x\rdata: 1\r\r");
        assert_eq!(b.len(), 2);
        assert_eq!((b[0].event.as_str(), b[0].data.as_str()), ("message", "a\nb"));
        assert_eq!((b[1].event.as_str(), b[1].data.as_str()), ("x", "1"));
        assert!(p.push(b"event: y\ndata: {}\n").is_empty());
    }
}
