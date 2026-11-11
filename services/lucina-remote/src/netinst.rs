//! The network side of `lucina-install-disk netinstall`: a small HTTP/1.1 client over `TcpStream`, the signed manifest's fetch and
//! checks, and [`VerifiedChunks`], the reader that gives `install()` the image only as the manifest allows.
//!
//! Plain HTTP: trust comes from the manifest's signature and the chunk hashes in it, not from the transport
//! (docs/design/lucina-net-installer.md, section 4). The decisions are `lucina_release::rules` and [`crate::netinst_rules`].

use std::{
    io::{self, Read, Write},
    net::TcpStream,
    thread,
    time::Duration,
};

use lucina_release::{Manifest, rules};
use sha2::{Digest, Sha256};

use crate::netinst_rules;

/// The most a manifest may be: a manifest of the largest image in the smallest chunks is under this.
const MAX_MANIFEST: u64 = 80 << 20;
/// How many times a chunk is fetched again after a network error, and the wait between.
pub const RETRIES: u32 = 20;
pub const RETRY_PAUSE: Duration = Duration::from_secs(3);
const IO_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug)]
pub enum FetchError {
    /// The connection failed, dropped or timed out: the same request may be tried again.
    Network(String),
    /// The server answered, and the answer is not one to try again.
    Refused(String),
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchError::Network(s) | FetchError::Refused(s) => f.write_str(s),
        }
    }
}

/// `http://host:port` (the port defaults to 80, no path), as `host:port`.
pub fn parse_server(server: &str) -> Result<String, String> {
    let rest = server
        .strip_prefix("http://")
        .ok_or_else(|| format!("--server {server}: expected http://<host>:<port>"))?
        .trim_end_matches('/');
    if rest.is_empty() || rest.contains(['/', '@', ' ']) {
        return Err(format!("--server {server}: expected http://<host>:<port>"));
    }
    Ok(if rest.contains(':') {
        rest.to_string()
    } else {
        format!("{rest}:80")
    })
}

/// What the status line and headers say, pure.
#[derive(Debug, PartialEq, Eq)]
pub struct Head {
    pub status: u16,
    pub length: Option<u64>,
}

pub fn parse_head(text: &str) -> Option<Head> {
    let mut lines = text.split("\r\n");
    let mut status = lines.next()?.split(' ');
    if !status.next()?.starts_with("HTTP/1.") {
        return None;
    }
    let code = status.next()?.parse().ok()?;
    let mut length = None;
    for l in lines {
        if let Some((k, v)) = l.split_once(':')
            && k.eq_ignore_ascii_case("content-length")
        {
            length = Some(v.trim().parse().ok()?);
        }
    }
    Some(Head {
        status: code,
        length,
    })
}

/// Whether an answer to a request for `want` bytes (`whole`: the range is the whole file) is acceptable: a 206, or a 200 for the whole.
pub fn answer_ok(head: &Head, want: u64, whole: bool) -> bool {
    (head.status == 206 || (head.status == 200 && whole)) && head.length == Some(want)
}

/// GET `path` on `addr` with a `Range` for bytes `start..start+len` (or none). The body is exactly the length asked for.
pub fn get(
    addr: &str,
    path: &str,
    range: Option<(u64, u64)>,
    whole: bool,
) -> Result<Vec<u8>, FetchError> {
    let net = |what: &str, e: io::Error| FetchError::Network(format!("{addr}{path}: {what}: {e}"));
    let mut s = TcpStream::connect(addr).map_err(|e| net("connect", e))?;
    s.set_read_timeout(Some(IO_TIMEOUT))
        .map_err(|e| net("timeout", e))?;
    s.set_write_timeout(Some(IO_TIMEOUT))
        .map_err(|e| net("timeout", e))?;
    let host = addr.split(':').next().unwrap_or(addr);
    let mut req = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nAccept-Encoding: identity\r\n"
    );
    if let Some((start, len)) = range {
        req += &format!("Range: bytes={start}-{}\r\n", start + len - 1);
    }
    req += "\r\n";
    s.write_all(req.as_bytes()).map_err(|e| net("send", e))?;
    // The head, a byte at a time up to the blank line, so that no body byte is lost to it.
    let mut head = Vec::new();
    let mut b = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() > 16384 {
            return Err(FetchError::Refused(format!(
                "{addr}{path}: the answer's head is too long"
            )));
        }
        match s.read(&mut b) {
            Ok(0) => {
                return Err(FetchError::Network(format!(
                    "{addr}{path}: closed before the head ended"
                )));
            }
            Ok(_) => head.push(b[0]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(net("read", e)),
        }
    }
    let text = String::from_utf8_lossy(&head[..head.len() - 4]).into_owned();
    let h = parse_head(&text)
        .ok_or_else(|| FetchError::Refused(format!("{addr}{path}: not an HTTP/1.x answer")))?;
    let want = range.map_or(h.length.unwrap_or(0), |(_, len)| len);
    if range.is_none() && (h.status != 200 || h.length.is_none_or(|n| n > MAX_MANIFEST)) {
        return Err(FetchError::Refused(format!(
            "{addr}{path}: answered {} ({:?} bytes)",
            h.status, h.length
        )));
    }
    if range.is_some() && !answer_ok(&h, want, whole) {
        return Err(FetchError::Refused(format!(
            "{addr}{path}: answered {} with {:?} bytes for a range of {want}",
            h.status, h.length
        )));
    }
    let mut body = vec![0u8; want as usize];
    s.read_exact(&mut body).map_err(|e| net("body", e))?;
    Ok(body)
}

/// Fetches `<server>/<board>/manifest`, checks its signature against the keys in `keys_text`, and the rules for this board and machine.
pub fn fetch_manifest(
    server: &str,
    board: &str,
    keys_text: &str,
    installed: Option<u64>,
    go_back: bool,
) -> Result<Manifest, String> {
    let addr = parse_server(server)?;
    let body = get(&addr, &format!("/{board}/manifest"), None, true).map_err(|e| e.to_string())?;
    let text = String::from_utf8(body).map_err(|_| "the manifest is not text".to_string())?;
    let keys = lucina_release::parse_keys(keys_text).map_err(|e| e.to_string())?;
    let m = lucina_release::verify(&text, &keys).map_err(|e| e.to_string())?;
    netinst_rules::check_manifest(&m, board, installed, go_back)?;
    Ok(m)
}

/// The image, chunk by chunk, as a [`Read`]. A byte of a chunk is given only after that chunk's sha256 matched the manifest, and
/// the last chunk only after the whole image's sha256 matched too, so whoever reads to the end has the whole, verified image
/// (and a reader that stops at an error never reached the commit).
pub struct VerifiedChunks {
    addr: String,
    path: String,
    m: Manifest,
    count: u64,
    /// Chunks verified and given out or being given out: the prefix of `rules::next_to_fetch`.
    prefix: u64,
    whole: Sha256,
    buf: Vec<u8>,
    pos: usize,
    failed: Option<String>,
    pause: Duration,
}

impl VerifiedChunks {
    pub fn new(server: &str, path: &str, m: Manifest) -> Result<VerifiedChunks, String> {
        let count =
            rules::chunk_count(m.size, u64::from(m.chunk)).ok_or("the manifest has no image")?;
        Ok(VerifiedChunks {
            addr: parse_server(server)?,
            path: path.to_string(),
            m,
            count,
            prefix: 0,
            whole: Sha256::new(),
            buf: Vec::new(),
            pos: 0,
            failed: None,
            pause: RETRY_PAUSE,
        })
    }

    /// A shorter wait between tries, for tests.
    pub fn with_pause(mut self, pause: Duration) -> VerifiedChunks {
        self.pause = pause;
        self
    }

    fn fetch_chunk(&self, i: u64) -> Result<Vec<u8>, String> {
        let (start, len) = rules::chunk_range(i, self.m.size, u64::from(self.m.chunk))
            .ok_or("chunk past the end")?;
        let whole = start == 0 && len == self.m.size;
        let mut tries = 0;
        loop {
            match get(&self.addr, &self.path, Some((start, len)), whole) {
                Ok(b) => return Ok(b),
                Err(FetchError::Refused(e)) => return Err(e),
                Err(FetchError::Network(e)) if tries >= RETRIES => {
                    return Err(format!("chunk {i}: {e}; given up after {RETRIES} tries"));
                }
                Err(FetchError::Network(e)) => {
                    tries += 1;
                    println!(
                        "netinstall: chunk {i}: {e}; try {tries} of {RETRIES} in {} s",
                        self.pause.as_secs()
                    );
                    thread::sleep(self.pause);
                }
            }
        }
    }

    /// The next chunk, fetched and checked, or the reason it cannot be given.
    fn next(&mut self) -> Result<Option<Vec<u8>>, String> {
        let Some(i) = rules::next_to_fetch(self.prefix, self.count) else {
            return Ok(None);
        };
        let chunk = self.fetch_chunk(i)?;
        let hash_ok = <[u8; 32]>::from(Sha256::digest(&chunk)) == self.m.chunks[i as usize];
        if !rules::may_write_chunk(i, self.prefix, hash_ok) {
            return Err(format!(
                "chunk {i} does not match the manifest's sha256: stopped, nothing of it was given"
            ));
        }
        let mut whole = self.whole.clone();
        whole.update(&chunk);
        let whole_ok = <[u8; 32]>::from(whole.clone().finalize()) == self.m.sha256;
        // The last chunk goes out only when the whole image matches; the others never need to.
        if i + 1 == self.count && !rules::may_make_bootable(i + 1, self.count, whole_ok) {
            return Err(
                "the whole image does not match the manifest's sha256: the last chunk is withheld"
                    .into(),
            );
        }
        self.whole = whole;
        self.prefix = i + 1;
        if i % 16 == 0 || i + 1 == self.count {
            println!("netinstall: chunk {} of {} verified", i + 1, self.count);
        }
        Ok(Some(chunk))
    }
}

impl Read for VerifiedChunks {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if let Some(e) = &self.failed {
            return Err(io::Error::other(e.clone()));
        }
        if self.pos == self.buf.len() {
            match self.next() {
                Ok(Some(c)) => {
                    self.buf = c;
                    self.pos = 0;
                }
                Ok(None) => return Ok(0),
                Err(e) => {
                    self.buf = Vec::new();
                    self.pos = 0;
                    self.failed = Some(e.clone());
                    return Err(io::Error::other(e));
                }
            }
        }
        let n = out.len().min(self.buf.len() - self.pos);
        out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::{BufRead, BufReader},
        net::TcpListener,
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
        },
    };

    use super::*;

    const C: usize = lucina_release::MIN_CHUNK as usize;

    #[derive(Default)]
    struct Server {
        /// The first request for the chunk starting at this byte gets half a body and a closed connection.
        drop_once_at: Option<u64>,
        dropped: AtomicBool,
        /// This byte of the image is served flipped.
        tamper_at: Option<usize>,
    }

    /// Serves `/b/disk-stream` (Range) and `/b/manifest` on 127.0.0.1; returns `http://127.0.0.1:port`.
    fn serve(image: Vec<u8>, manifest: String, cfg: Server) -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        let cfg = Arc::new(cfg);
        let image = Arc::new(image);
        let manifest = Arc::new(Mutex::new(manifest));
        thread::spawn(move || {
            for s in l.incoming() {
                let (cfg, image, manifest) = (cfg.clone(), image.clone(), manifest.clone());
                thread::spawn(move || {
                    let mut s = s.unwrap();
                    let mut r = BufReader::new(s.try_clone().unwrap());
                    let (mut path, mut range) = (String::new(), None);
                    let mut line = String::new();
                    while r.read_line(&mut line).unwrap_or(0) > 0 && line != "\r\n" {
                        if let Some(p) = line.strip_prefix("GET ") {
                            path = p.split(' ').next().unwrap().to_string();
                        }
                        if let Some(v) = line.to_ascii_lowercase().strip_prefix("range: bytes=") {
                            let (a, b) = v.trim().split_once('-').unwrap();
                            range =
                                Some((a.parse::<usize>().unwrap(), b.parse::<usize>().unwrap()));
                        }
                        line.clear();
                    }
                    if path.ends_with("/manifest") {
                        let m = manifest.lock().unwrap().clone();
                        let _ = write!(
                            s,
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{m}",
                            m.len()
                        );
                    } else if let (true, Some((a, z))) = (path.ends_with("/disk-stream"), range) {
                        let mut body = image[a..=z].to_vec();
                        if let Some(t) = cfg.tamper_at.filter(|t| (a..=z).contains(t)) {
                            body[t - a] ^= 1;
                        }
                        let whole = a == 0 && z + 1 == image.len();
                        let (code, extra) = if whole {
                            ("200 OK", String::new())
                        } else {
                            (
                                "206 Partial Content",
                                format!("Content-Range: bytes {a}-{z}/{}\r\n", image.len()),
                            )
                        };
                        let _ = write!(
                            s,
                            "HTTP/1.1 {code}\r\n{extra}Content-Length: {}\r\n\r\n",
                            body.len()
                        );
                        if cfg.drop_once_at == Some(a as u64)
                            && !cfg.dropped.swap(true, Ordering::SeqCst)
                        {
                            let _ = s.write_all(&body[..body.len() / 2]);
                            return;
                        }
                        let _ = s.write_all(&body);
                    } else {
                        let _ = write!(s, "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
                    }
                });
            }
        });
        url
    }

    fn image(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i * 31 % 251) as u8).collect()
    }

    fn manifest_of(img: &[u8], seq: u64, board: &str) -> Manifest {
        Manifest::make(
            board,
            &"c".repeat(40),
            "2026-11-11T11:39:59Z",
            seq,
            C as u32,
            img,
        )
        .unwrap()
    }

    fn drain(r: &mut VerifiedChunks) -> (Vec<u8>, Option<String>) {
        let mut got = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            match r.read(&mut buf) {
                Ok(0) => return (got, None),
                Ok(n) => got.extend_from_slice(&buf[..n]),
                Err(e) => return (got, Some(e.to_string())),
            }
        }
    }

    fn reader(url: &str, m: Manifest) -> VerifiedChunks {
        VerifiedChunks::new(url, "/b/disk-stream", m)
            .unwrap()
            .with_pause(Duration::from_millis(5))
    }

    #[test]
    fn good_run_gives_the_exact_bytes() {
        for len in [C, 3 * C + 17, 2 * C] {
            let img = image(len);
            let url = serve(img.clone(), String::new(), Server::default());
            let (got, err) = drain(&mut reader(&url, manifest_of(&img, 1, "x")));
            assert_eq!(err, None);
            assert!(got == img, "length {len}");
        }
    }

    #[test]
    fn tampered_chunk_is_never_given() {
        let img = image(3 * C + 17);
        let url = serve(
            img.clone(),
            String::new(),
            Server {
                tamper_at: Some(C + 5),
                ..Server::default()
            },
        );
        let (got, err) = drain(&mut reader(&url, manifest_of(&img, 1, "x")));
        assert!(err.unwrap().contains("chunk 1"));
        assert!(got == img[..C], "only chunk 0 was given");
    }

    #[test]
    fn dropped_connection_is_resumed() {
        let img = image(3 * C + 17);
        let url = serve(
            img.clone(),
            String::new(),
            Server {
                drop_once_at: Some(C as u64),
                ..Server::default()
            },
        );
        let (got, err) = drain(&mut reader(&url, manifest_of(&img, 1, "x")));
        assert_eq!(err, None);
        assert!(got == img);
    }

    #[test]
    fn a_server_that_is_gone_ends_in_an_error() {
        let img = image(2 * C);
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let dead = format!("http://{}", l.local_addr().unwrap());
        drop(l);
        let mut r = reader(&dead, manifest_of(&img, 1, "x"));
        r.pause = Duration::from_millis(1);
        let (got, err) = drain(&mut r);
        assert!(got.is_empty() && err.unwrap().contains("given up"));
        // The failure sticks.
        assert!(r.read(&mut [0u8; 8]).is_err());
    }

    #[test]
    fn wrong_whole_hash_withholds_the_last_chunk() {
        let img = image(3 * C + 17);
        let mut m = manifest_of(&img, 1, "x");
        m.sha256[0] ^= 1;
        let url = serve(img.clone(), String::new(), Server::default());
        let (got, err) = drain(&mut reader(&url, m));
        assert!(err.unwrap().contains("whole image"));
        assert!(got == img[..3 * C], "the last chunk was not given");
    }

    const SEED: [u8; 32] = [9; 32];

    fn signed(m: &Manifest) -> (String, String) {
        let k = lucina_release::signing_key_from_seed(&lucina_release::b64_encode(&SEED)).unwrap();
        (
            m.sign(&k),
            lucina_release::b64_encode(&k.verifying_key().to_bytes()),
        )
    }

    fn check(
        m: &Manifest,
        board: &str,
        installed: Option<u64>,
        go_back: bool,
    ) -> Result<u64, String> {
        let (text, keys) = signed(m);
        let url = serve(Vec::new(), text, Server::default());
        fetch_manifest(&url, board, &keys, installed, go_back).map(|m| m.seq)
    }

    #[test]
    fn manifest_checks() {
        let b = "galaxy-book-go";
        let img = image(C + 3);
        let mut m = manifest_of(&img, 5, b);
        m.writes = vec!["lun0:esp".into(), "lun0:root-a".into()];
        assert_eq!(check(&m, b, None, false), Ok(5));
        assert_eq!(check(&m, b, Some(5), false), Ok(5));
        // A lower sequence needs --go-back.
        assert!(check(&m, b, Some(6), false).unwrap_err().contains("lower"));
        assert_eq!(check(&m, b, Some(6), true), Ok(5));
        // A target outside the compiled set.
        let mut bad = m.clone();
        bad.writes.push("lun1:x".into());
        assert!(check(&bad, b, None, true).unwrap_err().contains("lun1:x"));
        // An unknown board, and a manifest for another board.
        let other = manifest_of(&img, 5, "lenovo-duet");
        assert!(check(&other, "lenovo-duet", None, true).is_err());
        assert!(
            check(&other, b, None, true)
                .unwrap_err()
                .contains("lenovo-duet")
        );
    }

    #[test]
    fn manifest_signature_is_checked() {
        let b = "galaxy-book-go";
        let m = manifest_of(&image(C), 1, b);
        let (text, keys) = signed(&m);
        // Another key, and a manifest changed after signing.
        let other =
            lucina_release::signing_key_from_seed(&lucina_release::b64_encode(&[1u8; 32])).unwrap();
        let other_keys = lucina_release::b64_encode(&other.verifying_key().to_bytes());
        let url = serve(Vec::new(), text.clone(), Server::default());
        assert!(fetch_manifest(&url, b, &other_keys, None, false).is_err());
        let url = serve(
            Vec::new(),
            text.replace("seq 1", "seq 2"),
            Server::default(),
        );
        assert!(fetch_manifest(&url, b, &keys, None, false).is_err());
    }

    #[test]
    fn head_and_server_parsing() {
        assert_eq!(
            parse_server("http://127.0.0.1:8080/"),
            Ok("127.0.0.1:8080".into())
        );
        assert_eq!(parse_server("http://buildhost"), Ok("buildhost:80".into()));
        assert!(parse_server("https://x:1").is_err());
        assert!(parse_server("http://x:1/path").is_err());
        assert!(parse_server("http://").is_err());
        let h = parse_head("HTTP/1.1 206 Partial Content\r\ncontent-length: 10\r\nX: y").unwrap();
        assert_eq!(
            h,
            Head {
                status: 206,
                length: Some(10)
            }
        );
        assert!(parse_head("garbage").is_none());
        assert!(answer_ok(&h, 10, false));
        assert!(!answer_ok(&h, 11, false));
        let ok = Head {
            status: 200,
            length: Some(10),
        };
        assert!(answer_ok(&ok, 10, true) && !answer_ok(&ok, 10, false));
        assert!(!answer_ok(
            &Head {
                status: 404,
                length: Some(10)
            },
            10,
            true
        ));
    }
}
