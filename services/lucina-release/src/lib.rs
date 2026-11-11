//! Signed per-board release manifests with chunk hashes, for the net installer (`docs/design/lucina-net-installer.md`, sections 4
//! and 5). The text format is canonical: one field per line, `\n` endings, and the signature covers every byte before the `sig`
//! line. [`rules`] holds the pure arithmetic the installer decides with; Verus proves it (`verification/net-installer`).

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use std::io::Read;

pub mod rules;
pub mod serve;

pub const MAGIC: &str = "LUCINA-MANIFEST 1";
pub const DEFAULT_CHUNK: u32 = 4 * 1024 * 1024;
pub const MIN_CHUNK: u32 = 65536;
pub const MAX_CHUNK: u32 = 67108864;

/// First 8 bytes of sha256 of a public key.
pub type KeyId = [u8; 8];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// A line that is missing, unknown, duplicated, out of order or malformed. Says which and why.
    Format(String),
    ChunkCount,
    EmptyImage,
    BadChunkSize,
    NotCanonical,
    UnknownKey(String),
    BadSignature,
    Io(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Format(s) => write!(f, "bad manifest: {s}"),
            Error::ChunkCount => f.write_str("bad manifest: chunk count does not match the size"),
            Error::EmptyImage => f.write_str("bad manifest: the image is empty"),
            Error::BadChunkSize => f.write_str(
                "bad manifest: chunk size must be a power of two from 65536 to 67108864",
            ),
            Error::NotCanonical => f.write_str("bad manifest: not in canonical form"),
            Error::UnknownKey(id) => write!(f, "signed by key {id}, which is not in the list"),
            Error::BadSignature => f.write_str("signature does not match"),
            Error::Io(s) => write!(f, "read error: {s}"),
        }
    }
}

impl std::error::Error for Error {}

fn fmt_err<T>(s: &str) -> Result<T, Error> {
    Err(Error::Format(s.to_string()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub board: String,
    pub commit: String,
    pub date: String,
    pub seq: u64,
    pub size: u64,
    pub sha256: [u8; 32],
    pub chunk: u32,
    pub chunks: Vec<[u8; 32]>,
    pub writes: Vec<String>,
}

pub fn hex(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        let _ = write!(s, "{x:02x}");
    }
    s
}

fn unhex<const N: usize>(s: &str) -> Option<[u8; N]> {
    if s.len() != N * 2
        || !s
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    {
        return None;
    }
    let mut out = [0u8; N];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

/// The key id of a public key: the first 8 bytes of sha256 of its 32 bytes.
pub fn key_id(key: &VerifyingKey) -> KeyId {
    let h = Sha256::digest(key.as_bytes());
    let mut id = [0u8; 8];
    id.copy_from_slice(&h[..8]);
    id
}

fn valid_board(s: &str) -> bool {
    (1..=40).contains(&s.len())
        && s.bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}

fn valid_target(s: &str) -> bool {
    (1..=64).contains(&s.len())
        && s.bytes().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'-' | b':' | b'_' | b'.')
        })
}

/// `YYYY-MM-DDTHH:MM:SSZ`, digits where digits go and fields in range.
fn valid_date(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 20 {
        return false;
    }
    for (i, &c) in b.iter().enumerate() {
        let ok = match i {
            4 | 7 => c == b'-',
            10 => c == b'T',
            13 | 16 => c == b':',
            19 => c == b'Z',
            _ => c.is_ascii_digit(),
        };
        if !ok {
            return false;
        }
    }
    let n = |a: usize, z: usize| s[a..z].parse::<u32>().unwrap_or(u32::MAX);
    let (mo, d, h, mi, se) = (n(5, 7), n(8, 10), n(11, 13), n(14, 16), n(17, 19));
    (1..=12).contains(&mo) && (1..=31).contains(&d) && h < 24 && mi < 60 && se <= 60
}

fn valid_chunk_size(c: u32) -> bool {
    c.is_power_of_two() && (MIN_CHUNK..=MAX_CHUNK).contains(&c)
}

impl Manifest {
    /// Streams the image once, computing every chunk's hash and the whole-image hash.
    pub fn make(
        board: &str,
        commit: &str,
        date: &str,
        seq: u64,
        chunk: u32,
        mut reader: impl Read,
    ) -> Result<Manifest, Error> {
        if !valid_chunk_size(chunk) {
            return Err(Error::BadChunkSize);
        }
        let mut whole = Sha256::new();
        let mut chunks = Vec::new();
        let mut size = 0u64;
        let mut buf = vec![0u8; chunk as usize];
        loop {
            let mut filled = 0;
            while filled < buf.len() {
                let n = reader
                    .read(&mut buf[filled..])
                    .map_err(|e| Error::Io(e.to_string()))?;
                if n == 0 {
                    break;
                }
                filled += n;
            }
            if filled == 0 {
                break;
            }
            whole.update(&buf[..filled]);
            chunks.push(Sha256::digest(&buf[..filled]).into());
            size += filled as u64;
            if filled < buf.len() {
                break;
            }
        }
        if size == 0 {
            return Err(Error::EmptyImage);
        }
        let m = Manifest {
            board: board.to_string(),
            commit: commit.to_string(),
            date: date.to_string(),
            seq,
            size,
            sha256: whole.finalize().into(),
            chunk,
            chunks,
            writes: Vec::new(),
        };
        m.check_fields()?;
        Ok(m)
    }

    fn check_fields(&self) -> Result<(), Error> {
        if !valid_board(&self.board) {
            return fmt_err("board must be [a-z0-9-]{1,40}");
        }
        if unhex::<20>(&self.commit).is_none() {
            return fmt_err("commit must be 40 lowercase hex");
        }
        if !valid_date(&self.date) {
            return fmt_err("date must be RFC 3339 UTC, like 2026-11-11T11:39:59Z");
        }
        if !valid_chunk_size(self.chunk) {
            return Err(Error::BadChunkSize);
        }
        match rules::chunk_count(self.size, u64::from(self.chunk)) {
            None => return Err(Error::EmptyImage),
            Some(n) if n != self.chunks.len() as u64 => return Err(Error::ChunkCount),
            Some(_) => {}
        }
        for (i, w) in self.writes.iter().enumerate() {
            if !valid_target(w) {
                return fmt_err("write target must be [a-z0-9:_.-]{1,64}");
            }
            if self.writes[..i].contains(w) {
                return fmt_err("duplicate write target");
            }
        }
        Ok(())
    }

    /// The canonical text, without the `sig` line. This is what the signature covers.
    pub fn render(&self) -> String {
        let mut s = String::new();
        let _ = writeln!(s, "{MAGIC}");
        let _ = writeln!(s, "board {}", self.board);
        let _ = writeln!(s, "commit {}", self.commit);
        let _ = writeln!(s, "date {}", self.date);
        let _ = writeln!(s, "seq {}", self.seq);
        let _ = writeln!(s, "size {}", self.size);
        let _ = writeln!(s, "sha256 {}", hex(&self.sha256));
        let _ = writeln!(s, "chunk {}", self.chunk);
        for (i, c) in self.chunks.iter().enumerate() {
            let _ = writeln!(s, "c {i} {}", hex(c));
        }
        for w in &self.writes {
            let _ = writeln!(s, "write {w}");
        }
        s
    }

    /// The whole signed manifest: the canonical text and the `sig` line.
    pub fn sign(&self, key: &SigningKey) -> String {
        let mut s = self.render();
        let sig = key.sign(s.as_bytes());
        let _ = writeln!(
            s,
            "sig {} {}",
            hex(&key_id(&key.verifying_key())),
            B64.encode(sig.to_bytes())
        );
        s
    }

    /// Reads an unsigned manifest (the canonical text without a `sig` line), as `make` prints it.
    pub fn parse_unsigned(text: &str) -> Result<Manifest, Error> {
        let m = parse_body(text)?;
        if m.render() != text {
            return Err(Error::NotCanonical);
        }
        Ok(m)
    }

    /// Reads a signed manifest. Returns it with the bytes the signature covers, the key id and the signature; it does not check the
    /// signature (see [`verify`]). Anything but the exact canonical form is refused.
    pub fn parse(text: &str) -> Result<(Manifest, &[u8], KeyId, Signature), Error> {
        let sig_at = match text.rfind("\nsig ") {
            Some(i) => i + 1,
            None => return fmt_err("no sig line"),
        };
        let (body, sig_line) = text.split_at(sig_at);
        let sig_line = match sig_line.strip_suffix('\n') {
            Some(l) if !l.contains('\n') => l,
            _ => return fmt_err("the sig line must be last, once, and end in a newline"),
        };
        let mut f = sig_line.split(' ');
        let (_, id, b64, extra) = (f.next(), f.next(), f.next(), f.next());
        let (id, b64) = match (id, b64, extra) {
            (Some(i), Some(b), None) => (i, b),
            _ => return fmt_err("sig line must be `sig <key id> <signature>`"),
        };
        let id: KeyId = match unhex(id) {
            Some(i) => i,
            None => return fmt_err("bad key id"),
        };
        let raw = B64
            .decode(b64)
            .map_err(|_| Error::Format("bad signature encoding".into()))?;
        let sig = Signature::from_slice(&raw)
            .map_err(|_| Error::Format("bad signature length".into()))?;
        let m = parse_body(body)?;
        if m.render() != body {
            return Err(Error::NotCanonical);
        }
        Ok((m, body.as_bytes(), id, sig))
    }
}

fn parse_body(body: &str) -> Result<Manifest, Error> {
    if body.contains('\r') || !body.ends_with('\n') {
        return fmt_err("lines must end in \\n only");
    }
    let mut lines = body[..body.len() - 1].split('\n');
    if lines.next() != Some(MAGIC) {
        return fmt_err("first line must be `LUCINA-MANIFEST 1`");
    }
    let mut field = |key: &str| -> Result<&str, Error> {
        let l = lines
            .next()
            .ok_or_else(|| Error::Format(format!("missing `{key}` line")))?;
        match l.strip_prefix(key).and_then(|r| r.strip_prefix(' ')) {
            Some(v) if !v.is_empty() && !v.starts_with(' ') && !v.ends_with(' ') => Ok(v),
            _ => Err(Error::Format(format!("expected `{key} <value>` line"))),
        }
    };
    let num = |s: &str, what: &str| -> Result<u64, Error> {
        s.parse::<u64>()
            .map_err(|_| Error::Format(format!("{what} is not a decimal number")))
    };
    let board = field("board")?.to_string();
    let commit = field("commit")?.to_string();
    let date = field("date")?.to_string();
    let seq = num(field("seq")?, "seq")?;
    let size = num(field("size")?, "size")?;
    let sha256 = unhex::<32>(field("sha256")?).ok_or_else(|| Error::Format("bad sha256".into()))?;
    let chunk = u32::try_from(num(field("chunk")?, "chunk")?).map_err(|_| Error::BadChunkSize)?;
    let mut chunks = Vec::new();
    let mut writes = Vec::new();
    for l in lines {
        if let Some(r) = l.strip_prefix("c ") {
            if !writes.is_empty() {
                return fmt_err("c lines must come before write lines");
            }
            let (idx, h) = r
                .split_once(' ')
                .ok_or_else(|| Error::Format("bad c line".into()))?;
            if num(idx, "chunk index")? != chunks.len() as u64 {
                return fmt_err("c lines must count up from 0 in order");
            }
            chunks.push(unhex::<32>(h).ok_or_else(|| Error::Format("bad chunk hash".into()))?);
        } else if let Some(t) = l.strip_prefix("write ") {
            writes.push(t.to_string());
        } else {
            return Err(Error::Format(format!(
                "unknown or misplaced line `{}`",
                l.chars().take(20).collect::<String>()
            )));
        }
    }
    let m = Manifest {
        board,
        commit,
        date,
        seq,
        size,
        sha256,
        chunk,
        chunks,
        writes,
    };
    m.check_fields()?;
    Ok(m)
}

/// Checks a signed manifest against a list of keys: the signature must be by one of them, matched by key id.
pub fn verify(text: &str, keys: &[VerifyingKey]) -> Result<Manifest, Error> {
    let (m, signed, id, sig) = Manifest::parse(text)?;
    let key = keys
        .iter()
        .find(|k| key_id(k) == id)
        .ok_or_else(|| Error::UnknownKey(hex(&id)))?;
    key.verify(signed, &sig).map_err(|_| Error::BadSignature)?;
    Ok(m)
}

/// Public keys, one base64 per line; blank lines and `#` comments are skipped.
pub fn parse_keys(text: &str) -> Result<Vec<VerifyingKey>, Error> {
    let mut out = Vec::new();
    for l in text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
    {
        let raw = B64
            .decode(l)
            .map_err(|_| Error::Format("bad public key encoding".into()))?;
        let arr: [u8; 32] = raw
            .try_into()
            .map_err(|_| Error::Format("public key must be 32 bytes".into()))?;
        out.push(
            VerifyingKey::from_bytes(&arr)
                .map_err(|_| Error::Format("not a valid public key".into()))?,
        );
    }
    Ok(out)
}

/// A signing key from a base64 32-byte seed.
pub fn signing_key_from_seed(b64: &str) -> Result<SigningKey, Error> {
    let raw = B64
        .decode(b64.trim())
        .map_err(|_| Error::Format("seed is not base64".into()))?;
    let seed: [u8; 32] = raw
        .try_into()
        .map_err(|_| Error::Format("seed must be 32 bytes".into()))?;
    Ok(SigningKey::from_bytes(&seed))
}

pub fn b64_encode(b: &[u8]) -> String {
    B64.encode(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
    const DATE: &str = "2026-11-11T11:39:59Z";

    fn key(n: u8) -> SigningKey {
        SigningKey::from_bytes(&[n; 32])
    }

    fn image(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i * 31 + 7) as u8).collect()
    }

    fn signed(len: usize, k: &SigningKey) -> String {
        let mut m =
            Manifest::make("juno-sc7180", COMMIT, DATE, 7, MIN_CHUNK, &image(len)[..]).unwrap();
        m.writes = vec!["lun0:root-a".into(), "lun0:esp".into()];
        m.sign(k)
    }

    #[test]
    fn round_trip() {
        let k = key(1);
        let text = signed(3 * MIN_CHUNK as usize + 7, &k);
        let m = verify(&text, &[key(9).verifying_key(), k.verifying_key()]).unwrap();
        assert_eq!(m.board, "juno-sc7180");
        assert_eq!(m.seq, 7);
        assert_eq!(m.size, 3 * u64::from(MIN_CHUNK) + 7);
        assert_eq!(m.chunks.len(), 4);
        assert_eq!(
            m.sha256,
            <[u8; 32]>::from(Sha256::digest(image(3 * MIN_CHUNK as usize + 7)))
        );
        assert_eq!(m.writes.len(), 2);
        assert!(text.ends_with('\n') && text.contains("\nsig "));
    }

    #[test]
    fn one_image_chunk_sizes() {
        for len in [
            1,
            MIN_CHUNK as usize - 1,
            MIN_CHUNK as usize,
            MIN_CHUNK as usize + 1,
        ] {
            let k = key(1);
            let m = verify(&signed(len, &k), &[k.verifying_key()]).unwrap();
            assert_eq!(
                m.chunks.len() as u64,
                rules::chunk_count(len as u64, u64::from(MIN_CHUNK)).unwrap()
            );
        }
    }

    #[test]
    fn tampering_any_byte_fails() {
        let k = key(1);
        let text = signed(MIN_CHUNK as usize + 3, &k);
        let keys = [k.verifying_key()];
        for i in 0..text.len() {
            let mut b = text.clone().into_bytes();
            b[i] = if b[i] == b'0' { b'1' } else { b'0' };
            if b[i] == text.as_bytes()[i] {
                continue;
            }
            let Ok(t) = String::from_utf8(b) else {
                continue;
            };
            assert!(
                verify(&t, &keys).is_err(),
                "byte {i} changed and still verified"
            );
        }
    }

    #[test]
    fn unlisted_key_fails() {
        let text = signed(100, &key(1));
        assert!(matches!(
            verify(&text, &[key(2).verifying_key()]),
            Err(Error::UnknownKey(_))
        ));
        assert!(matches!(verify(&text, &[]), Err(Error::UnknownKey(_))));
    }

    #[test]
    fn wrong_signature_with_right_key_id_fails() {
        let k = key(1);
        let text = signed(100, &k);
        let other = signed(101, &k);
        let sig = |t: &str| t.rsplit_once("\nsig ").unwrap().1.to_string();
        let forged = format!(
            "{}\nsig {}",
            text.rsplit_once("\nsig ").unwrap().0,
            sig(&other)
        );
        assert_eq!(
            verify(&forged, &[k.verifying_key()]),
            Err(Error::BadSignature)
        );
    }

    #[test]
    fn chunk_count_must_match_size() {
        let k = key(1);
        let mut m = Manifest::make(
            "b",
            COMMIT,
            DATE,
            1,
            MIN_CHUNK,
            &image(2 * MIN_CHUNK as usize)[..],
        )
        .unwrap();
        m.chunks.pop();
        // The signer signs whatever it is given; the reader refuses it.
        let text = m.sign(&k);
        assert_eq!(verify(&text, &[k.verifying_key()]), Err(Error::ChunkCount));
        let mut m = Manifest::make("b", COMMIT, DATE, 1, MIN_CHUNK, &image(10)[..]).unwrap();
        m.size = 3 * u64::from(MIN_CHUNK);
        assert_eq!(
            verify(&m.sign(&k), &[k.verifying_key()]),
            Err(Error::ChunkCount)
        );
    }

    #[test]
    fn malformed_manifests_are_refused() {
        let k = key(1);
        let good = signed(100, &k);
        let keys = [k.verifying_key()];
        let cases = [
            good.replace("\n", "\r\n"),
            good.replace("seq 7", "seq 007"),
            good.replace("board juno-sc7180", "board juno-sc7180 "),
            good.replace("board juno-sc7180\ncommit", "commit"),
            good.replace("write lun0:esp\n", "write lun0:esp\nwrite lun0:esp\n"),
            good.replace("size 100", "size 0"),
            good.replace("LUCINA-MANIFEST 1", "LUCINA-MANIFEST 2"),
            good.replace("chunk 65536\n", "chunk 65537\n"),
            good.replace("c 0 ", "c 1 "),
            format!("{good}extra\n"),
            good.trim_end().to_string(),
            good.replace("\nsig ", "\nx 1\nsig "),
            good.replace(
                "write lun0:root-a\nwrite lun0:esp\n",
                "write lun0:esp\nc 1 00\n",
            ),
        ];
        for (i, c) in cases.iter().enumerate() {
            assert!(verify(c, &keys).is_err(), "case {i} was accepted");
        }
        // Two sig lines.
        let sig_line = good.rsplit_once("\nsig ").unwrap().1;
        assert!(verify(&format!("{good}sig {sig_line}"), &keys).is_err());
    }

    #[test]
    fn make_refuses_bad_input() {
        assert_eq!(
            Manifest::make("b", COMMIT, DATE, 1, MIN_CHUNK, &b""[..]),
            Err(Error::EmptyImage)
        );
        assert_eq!(
            Manifest::make("b", COMMIT, DATE, 1, 1000, &b"x"[..]),
            Err(Error::BadChunkSize)
        );
        assert!(Manifest::make("B", COMMIT, DATE, 1, MIN_CHUNK, &b"x"[..]).is_err());
        assert!(Manifest::make("b", "abc", DATE, 1, MIN_CHUNK, &b"x"[..]).is_err());
        assert!(Manifest::make("b", COMMIT, "2026-11-11", 1, MIN_CHUNK, &b"x"[..]).is_err());
    }

    #[test]
    fn key_files() {
        let k = key(3).verifying_key();
        let text = format!("# release key\n\n{}\n", b64_encode(k.as_bytes()));
        assert_eq!(parse_keys(&text).unwrap(), vec![k]);
        assert!(parse_keys("!!!").is_err());
    }
}
