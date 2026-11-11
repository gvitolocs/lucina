//! `lucina-release serve`: the LAN file server the net installer downloads from
//! (`docs/design/lucina-net-installer.md` sections 3 and 4). It hands out the files of a release folder as they are,
//! with byte ranges so a chunk download reads no more of the image than the chunk itself. Trust comes from the signed
//! manifest, not from here: plain HTTP, std only, no dependencies.
//!
//! One thread per connection, one request per connection, `Connection: close`. The stick is the only client, and it
//! asks for one chunk at a time.

use std::io::{BufRead, BufReader, BufWriter, ErrorKind, Read, Seek, SeekFrom, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// A client that stops mid-header holds its thread for this long, and no other client waits for it.
const HEAD_TIMEOUT: Duration = Duration::from_secs(30);
/// A header this big is not a request.
const MAX_HEAD: usize = 16 * 1024;
/// How often the accept loop looks at the stop flag while it waits for a connection.
const POLL: Duration = Duration::from_millis(20);

#[derive(Debug, PartialEq, Eq)]
enum Range {
    Full,
    Part { start: u64, end: u64 },
    Unsatisfiable,
}

/// `bytes=a-b` and `bytes=a-` against a file of `size` bytes. Anything else (another unit, a list of ranges, a suffix
/// range) is not understood and ignored, which means the whole file: a client that cannot say what it wants gets
/// everything, never a part it did not ask for.
fn parse_range(v: &str, size: u64) -> Range {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit());
    let Some(spec) = v.trim().strip_prefix("bytes=") else {
        return Range::Full;
    };
    if spec.contains(',') {
        return Range::Full;
    }
    let Some((a, b)) = spec.split_once('-') else {
        return Range::Full;
    };
    let Ok(start) = a.parse::<u64>() else {
        return Range::Full;
    };
    if start >= size {
        return Range::Unsatisfiable;
    }
    if b.is_empty() {
        return Range::Part {
            start,
            end: size - 1,
        };
    }
    if !digits(b) {
        return Range::Full;
    }
    match b.parse::<u64>() {
        Ok(end) if end < start => Range::Unsatisfiable,
        Ok(end) => Range::Part {
            start,
            end: end.min(size - 1),
        },
        Err(_) => Range::Full,
    }
}

fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            if i + 3 > b.len() {
                return None;
            }
            let v = u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).ok()?, 16).ok()?;
            out.push(v);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Why a path gets no answer: the status, the reason phrase, and the one line to say it.
#[derive(Debug, PartialEq, Eq)]
struct Refused {
    status: u16,
    reason: &'static str,
    msg: &'static str,
}

const NO_SUCH: Refused = Refused {
    status: 404,
    reason: "Not Found",
    msg: "no such file",
};

/// The file inside `dir` that `target` names, or the status that says why it does not.
///
/// The target has to be origin-form (`/x`), so what is left of it is never an absolute path; `..`, in any form encoded
/// or not, is refused outright; and what is left may not canonicalize out of `dir`, which covers a symlink planted
/// inside it.
fn resolve(dir: &Path, target: &str) -> Result<PathBuf, Refused> {
    let refused = |status: u16, reason, msg| {
        Err(Refused {
            status,
            reason,
            msg,
        })
    };
    let Some(rest) = target.strip_prefix('/') else {
        return refused(400, "Bad Request", "ask for a path like /manifest");
    };
    let rest = match rest.split_once(['?', '#']) {
        Some((p, _)) => p,
        None => rest,
    };
    let decoded = match percent_decode(rest) {
        Some(d) => d,
        None => return refused(400, "Bad Request", "a broken % escape"),
    };
    if decoded.bytes().any(|c| c < b' ') {
        return refused(400, "Bad Request", "a control byte in the path");
    }
    let mut rel = PathBuf::new();
    for c in Path::new(&decoded).components() {
        match c {
            Component::Normal(s) => rel.push(s),
            Component::CurDir => {}
            _ => return refused(403, "Forbidden", "that path leaves the folder"),
        }
    }
    if rel.as_os_str().is_empty() {
        return Err(NO_SUCH);
    }
    let base = match dir.canonicalize() {
        Ok(b) => b,
        Err(_) => return refused(500, "Internal Server Error", "the folder is gone"),
    };
    let found = match dir.join(&rel).canonicalize() {
        Ok(f) => f,
        Err(_) => return Err(NO_SUCH),
    };
    if !found.starts_with(&base) {
        return refused(403, "Forbidden", "that path leaves the folder");
    }
    Ok(found)
}

struct Head {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
}

fn bad(msg: &str) -> std::io::Error {
    std::io::Error::new(ErrorKind::InvalidInput, msg.to_string())
}

/// Reads the request line and the headers of one request. Ok(None): the client hung up without asking for anything.
fn read_head<R: Read>(rd: &mut BufReader<R>) -> std::io::Result<Option<Head>> {
    let mut lines: Vec<String> = Vec::new();
    let mut total = 0usize;
    loop {
        let mut raw = Vec::new();
        let n = rd.read_until(b'\n', &mut raw)?;
        if n == 0 {
            return if lines.is_empty() {
                Ok(None)
            } else {
                Err(bad("the request ended mid-header"))
            };
        }
        total += n;
        if total > MAX_HEAD {
            return Err(bad("the request header is too big"));
        }
        if raw.last() != Some(&b'\n') {
            return Err(bad("a request line with no end"));
        }
        raw.pop();
        if raw.last() == Some(&b'\r') {
            raw.pop();
        }
        if raw.iter().any(|&c| c < b' ' && c != b'\t') {
            return Err(bad("a control byte in the request header"));
        }
        let line = String::from_utf8(raw).map_err(|_| bad("a request that is not UTF-8"))?;
        if line.is_empty() {
            break;
        }
        lines.push(line);
    }
    let request = lines
        .first()
        .ok_or_else(|| bad("no request line"))?
        .split(' ')
        .collect::<Vec<_>>();
    let [method, target, version] = request.as_slice() else {
        return Err(bad("the request line is not `METHOD /path HTTP/1.x`"));
    };
    if !version.starts_with("HTTP/1") {
        return Err(bad("only HTTP/1.x"));
    }
    let mut headers = Vec::new();
    for l in &lines[1..] {
        let Some((k, v)) = l.split_once(':') else {
            return Err(bad("a header line with no `:`"));
        };
        headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
    }
    Ok(Some(Head {
        method: method.to_string(),
        target: target.to_string(),
        headers,
    }))
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

fn send_headers(
    w: &mut impl Write,
    status: u16,
    reason: &str,
    len: u64,
    extra: &[(&str, String)],
) -> std::io::Result<()> {
    write!(
        w,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/octet-stream\r\n\
         Content-Length: {len}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n"
    )?;
    for (k, v) in extra {
        write!(w, "{k}: {v}\r\n")?;
    }
    write!(w, "\r\n")
}

/// A status with a one-line body saying what happened. `body` false is HEAD: the headers only, and the length it would
/// have had. Returns the status and the bytes actually written.
fn refuse(
    w: &mut impl Write,
    body: bool,
    status: u16,
    reason: &str,
    msg: &str,
    extra: &[(&str, String)],
) -> std::io::Result<(u16, u64)> {
    let text = format!("{msg}\n");
    let len = if body { text.len() as u64 } else { 0 };
    send_headers(w, status, reason, len, extra)?;
    if body {
        w.write_all(text.as_bytes())?;
    }
    Ok((status, len))
}

fn route(w: &mut impl Write, dir: &Path, head: &Head) -> std::io::Result<(u16, u64)> {
    let get = head.method == "GET";
    if head.method != "GET" && head.method != "HEAD" {
        return refuse(
            w,
            get,
            405,
            "Method Not Allowed",
            "GET or HEAD only",
            &[("Allow", "GET, HEAD".into())],
        );
    }
    let path = match resolve(dir, &head.target) {
        Ok(p) => p,
        Err(r) => return refuse(w, get, r.status, r.reason, r.msg, &[]),
    };
    let meta = match std::fs::metadata(&path) {
        Ok(m) if m.is_file() => m,
        _ => return refuse(w, get, 404, "Not Found", NO_SUCH.msg, &[]),
    };
    let size = meta.len();
    let (status, start, len) = match header(&head.headers, "range") {
        Some(v) => match parse_range(v, size) {
            Range::Full => (200u16, 0u64, size),
            Range::Part { start, end } => (206, start, end - start + 1),
            Range::Unsatisfiable => {
                return refuse(
                    w,
                    get,
                    416,
                    "Range Not Satisfiable",
                    "nothing of that file is in that range",
                    &[("Content-Range", format!("bytes */{size}"))],
                );
            }
        },
        None => (200, 0, size),
    };
    let mut extra: Vec<(&str, String)> = Vec::new();
    if status == 206 {
        let last = start + len - 1;
        extra.push(("Content-Range", format!("bytes {start}-{last}/{size}")));
    }
    send_headers(
        w,
        status,
        if status == 206 {
            "Partial Content"
        } else {
            "OK"
        },
        len,
        &extra,
    )?;
    if !get || len == 0 {
        return Ok((status, 0));
    }
    let mut f = std::fs::File::open(&path)?;
    if start > 0 {
        f.seek(SeekFrom::Start(start))?;
    }
    Ok((status, std::io::copy(&mut f.take(len), w)?))
}

fn handle(stream: &mut TcpStream, dir: &Path) -> std::io::Result<()> {
    let peer = stream
        .peer_addr()
        .map_or_else(|_| "-".to_string(), |p| p.to_string());
    let _ = stream.set_nodelay(true);
    let _ = stream.set_read_timeout(Some(HEAD_TIMEOUT));
    let _ = stream.set_write_timeout(Some(HEAD_TIMEOUT));
    let reader = stream.try_clone()?;
    let head = read_head(&mut BufReader::new(reader));
    let mut out = BufWriter::new(stream);
    let (status, sent, note) = match head {
        Ok(None) => return Ok(()),
        Ok(Some(head)) => {
            let note = format!("{} {}", head.method, head.target);
            match route(&mut out, dir, &head) {
                Ok(v) => (v.0, v.1, note),
                Err(e) => {
                    let _ = out.flush();
                    eprintln!("serve: {peer} {note} 0 0 io: {e}");
                    return Ok(());
                }
            }
        }
        Err(e) => {
            let (status, sent) = refuse(
                &mut out,
                true,
                400,
                "Bad Request",
                "not a request we know",
                &[],
            )?;
            (status, sent, format!("bad request: {e}"))
        }
    };
    out.flush()?;
    eprintln!("serve: {peer} {note} {status} {sent}");
    Ok(())
}

/// Serves `dir` on `addr` until the process ends.
pub fn serve(dir: &Path, addr: SocketAddr) -> std::io::Result<()> {
    let listener = TcpListener::bind(addr)?;
    eprintln!(
        "serve: {} on http://{}/",
        dir.display(),
        listener.local_addr()?
    );
    let stop = AtomicBool::new(false);
    serve_on(listener, dir, &stop)
}

/// The same on a listener someone else bound, and it returns once `stop` is set: what the tests use.
pub fn serve_on(listener: TcpListener, dir: &Path, stop: &AtomicBool) -> std::io::Result<()> {
    listener.set_nonblocking(true)?;
    let mut jobs: Vec<JoinHandle<()>> = Vec::new();
    while !stop.load(Ordering::Relaxed) {
        jobs.retain(|j| !j.is_finished());
        match listener.accept() {
            Ok((mut stream, _)) => {
                if stream.set_nonblocking(false).is_err() {
                    continue;
                }
                let dir = dir.to_path_buf();
                jobs.push(thread::spawn(move || {
                    if let Err(e) = handle(&mut stream, &dir) {
                        eprintln!("serve: {e}");
                    }
                }));
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => thread::sleep(POLL),
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    for j in jobs {
        let _ = j.join();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body() -> Vec<u8> {
        (0..1000).map(|i| (i % 251) as u8).collect()
    }

    struct Server {
        addr: SocketAddr,
        stop: std::sync::Arc<AtomicBool>,
        dir: PathBuf,
        outside: PathBuf,
    }

    /// A temp dir with `manifest` (1000 bytes) and `sub/nested`, and a file beside it that must never be read.
    fn start(tag: &str) -> Server {
        let parent = std::env::temp_dir();
        let dir = parent.join(format!("lucina-serve-{}-{tag}", std::process::id()));
        let outside = parent.join(format!("outside-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("manifest"), body()).unwrap();
        std::fs::write(dir.join("sub/nested"), b"nested").unwrap();
        std::fs::write(&outside, b"do not read").unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let (thread_stop, thread_dir) = (std::sync::Arc::clone(&stop), dir.clone());
        thread::spawn(move || {
            if let Err(e) = serve_on(listener, &thread_dir, &thread_stop) {
                eprintln!("serve_on: {e}");
            }
        });
        Server {
            addr,
            stop,
            dir,
            outside,
        }
    }

    impl Drop for Server {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            let _ = std::fs::remove_dir_all(&self.dir);
            let _ = std::fs::remove_file(&self.outside);
        }
    }

    /// Sends `request` raw and returns the response split at the empty line.
    fn ask(addr: SocketAddr, request: &str) -> (String, Vec<u8>) {
        let mut s = TcpStream::connect(addr).unwrap();
        s.write_all(request.as_bytes()).unwrap();
        let mut all = Vec::new();
        s.read_to_end(&mut all).unwrap();
        let i = all
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .unwrap_or_else(|| panic!("no header end in {all:?}"));
        (
            String::from_utf8_lossy(&all[..i]).into(),
            all[i + 4..].to_vec(),
        )
    }

    fn get(addr: SocketAddr, path: &str, range: Option<&str>) -> (String, Vec<u8>) {
        let range = range.map_or(String::new(), |v| format!("Range: {v}\r\n"));
        ask(
            addr,
            &format!("GET {path} HTTP/1.1\r\nHost: buildhost\r\n{range}Connection: close\r\n\r\n"),
        )
    }

    fn head_of(addr: SocketAddr, path: &str) -> String {
        ask(addr, &format!("GET {path} HTTP/1.1\r\n\r\n"))
            .0
            .lines()
            .next()
            .unwrap_or("")
            .to_string()
    }

    #[test]
    fn whole_files_and_byte_ranges() {
        let s = start("read");
        let want = body();
        let (h, b) = get(s.addr, "/manifest", None);
        assert!(h.starts_with("HTTP/1.1 200 OK"), "{h}");
        assert!(h.contains("\r\nContent-Length: 1000"), "{h}");
        assert!(h.contains("\r\nAccept-Ranges: bytes"), "{h}");
        assert_eq!(b, want);

        let (h, b) = get(s.addr, "/manifest", Some("bytes=10-19"));
        assert!(h.starts_with("HTTP/1.1 206 "), "{h}");
        assert!(h.contains("\r\nContent-Range: bytes 10-19/1000"), "{h}");
        assert!(h.contains("\r\nContent-Length: 10"), "{h}");
        assert_eq!(b, &want[10..20]);

        let (h, b) = get(s.addr, "/manifest", Some("bytes=990-"));
        assert!(h.contains("\r\nContent-Range: bytes 990-999/1000"), "{h}");
        assert_eq!(b, &want[990..]);

        let (h, b) = get(s.addr, "/manifest", Some("bytes=995-5000"));
        assert!(h.contains("\r\nContent-Length: 5"), "{h}");
        assert!(h.contains("\r\nContent-Range: bytes 995-999/1000"), "{h}");
        assert_eq!(b, &want[995..]);

        let (_, b) = get(s.addr, "/manifest", Some("bytes=999-999"));
        assert_eq!(b, &want[999..]);

        let (h, b) = get(s.addr, "/sub/nested", None);
        assert!(h.starts_with("HTTP/1.1 200 OK"), "{h}");
        assert_eq!(b, b"nested");
        let (h, b) = get(s.addr, "/sub/nested", Some("bytes=0-1"));
        assert!(h.starts_with("HTTP/1.1 206 "), "{h}");
        assert_eq!(b, b"ne");
    }

    #[test]
    fn head_gets_headers_and_no_body() {
        let s = start("head");
        let (h, b) = ask(s.addr, "HEAD /manifest HTTP/1.1\r\nHost: buildhost\r\n\r\n");
        assert!(h.starts_with("HTTP/1.1 200 OK"), "{h}");
        assert!(h.contains("\r\nContent-Length: 1000"), "{h}");
        assert!(b.is_empty(), "HEAD sent a body");
        let (h, b) = ask(
            s.addr,
            "HEAD /manifest HTTP/1.1\r\nRange: bytes=4-8\r\n\r\n",
        );
        assert!(h.starts_with("HTTP/1.1 206 "), "{h}");
        assert!(h.contains("\r\nContent-Length: 5"), "{h}");
        assert!(h.contains("\r\nContent-Range: bytes 4-8/1000"), "{h}");
        assert!(b.is_empty(), "HEAD of a range sent a body");
    }

    #[test]
    fn missing_refused_and_unsatisfiable() {
        let s = start("codes");
        assert_eq!(head_of(s.addr, "/nope"), "HTTP/1.1 404 Not Found");
        assert_eq!(head_of(s.addr, "/sub"), "HTTP/1.1 404 Not Found");
        assert_eq!(head_of(s.addr, "/"), "HTTP/1.1 404 Not Found");
        assert_eq!(head_of(s.addr, "/gone/manifest"), "HTTP/1.1 404 Not Found");
        let (h, b) = get(s.addr, "/manifest", Some("bytes=1000-"));
        assert!(h.starts_with("HTTP/1.1 416 "), "{h}");
        assert!(h.contains("\r\nContent-Range: bytes */1000"), "{h}");
        assert!(
            b.starts_with(b"nothing"),
            "{:?}",
            String::from_utf8_lossy(&b)
        );
        for r in [
            "bytes=1000-2000",
            "bytes=50-40",
            "bytes=99999-",
            "bytes=1000-1000",
        ] {
            let (h, _) = get(s.addr, "/manifest", Some(r));
            assert!(h.starts_with("HTTP/1.1 416 "), "{r}: {h}");
        }
        // A range we do not speak is no range at all: the whole file.
        for r in [
            "bytes=-10",
            "items=0-1",
            "bytes=0-1,4-5",
            "bytes=x-y",
            "nope",
            "bytes=",
        ] {
            let (h, b) = get(s.addr, "/manifest", Some(r));
            assert!(h.starts_with("HTTP/1.1 200 OK"), "{r}: {h}");
            assert_eq!(b.len(), 1000, "{r}");
        }
        for (req, why) in [
            ("POST /manifest HTTP/1.1\r\n\r\n", "405"),
            ("PUT /manifest HTTP/1.1\r\n\r\n", "405"),
            ("NOPE /manifest HTTP/1.1\r\n\r\n", "405"),
        ] {
            let (h, _) = ask(s.addr, req);
            assert!(h.starts_with(&format!("HTTP/1.1 {why} ")), "{why}: {h}");
            assert!(h.contains("\r\nAllow: GET, HEAD"), "{why}: {h}");
        }
        for req in [
            "GET manifest HTTP/1.1\r\n\r\n",
            "GET http://buildhost/manifest HTTP/1.1\r\n\r\n",
            "GET /manifest HTTP/0.9\r\n\r\n",
            "GARBAGE\r\n\r\n",
            "GET /a b HTTP/1.1\r\n\r\n",
            "GET /manifest\r\n\r\n",
            "GET /manifest HTTP/1.1\r\nno colon here\r\n\r\n",
        ] {
            let (h, _) = ask(s.addr, req);
            assert!(h.starts_with("HTTP/1.1 400 "), "{req} -> {h}");
        }
        // A closed connection with nothing on it is not an error, just nobody home.
        let _ = TcpStream::connect(s.addr);
    }

    #[test]
    fn paths_that_climb_out_are_refused() {
        let s = start("escape");
        let out = s.outside.file_name().unwrap().to_str().unwrap().to_string();
        for p in [
            format!("/../{out}"),
            format!("/sub/../../{out}"),
            format!("/%2e%2e/{out}"),
            format!("/%2E%2E%2F{out}"),
            "/..%2f..%2fetc/hostname".to_string(),
            format!("/./../{out}"),
            format!("//../{out}"),
            "/sub/%2e%2e%2f%2e%2e/etc/hostname".to_string(),
            "/%2e%2e%2f%2e%2e%2fmanifest".to_string(),
        ] {
            let (h, b) = ask(s.addr, &format!("GET {p} HTTP/1.1\r\n\r\n"));
            assert!(
                h.starts_with("HTTP/1.1 403 ") || h.starts_with("HTTP/1.1 404 "),
                "{p} -> {h}"
            );
            assert_ne!(b, body(), "{p} was served");
            assert_ne!(b, b"do not read", "{p} read the file outside");
        }
        // The same files, asked for the way they are: served.
        assert_eq!(head_of(s.addr, "/manifest"), "HTTP/1.1 200 OK");
        assert_eq!(head_of(s.addr, "/sub/nested"), "HTTP/1.1 200 OK");
        assert_eq!(head_of(s.addr, "/%73ub/nested"), "HTTP/1.1 200 OK");
        assert_eq!(head_of(s.addr, "/./sub/nested"), "HTTP/1.1 200 OK");
    }

    #[test]
    fn range_and_path_parsing_without_a_socket() {
        assert_eq!(
            parse_range("bytes=0-0", 10),
            Range::Part { start: 0, end: 0 }
        );
        assert_eq!(
            parse_range("bytes=3-", 10),
            Range::Part { start: 3, end: 9 }
        );
        assert_eq!(
            parse_range("bytes=0-99", 10),
            Range::Part { start: 0, end: 9 }
        );
        assert_eq!(parse_range("bytes=10-", 10), Range::Unsatisfiable);
        assert_eq!(parse_range("bytes=4-3", 10), Range::Unsatisfiable);
        assert_eq!(parse_range("bytes=0-", 0), Range::Unsatisfiable);
        assert_eq!(parse_range("bytes=-3", 10), Range::Full);
        assert_eq!(parse_range("", 10), Range::Full);
        assert_eq!(parse_range("bytes=-", 10), Range::Full);
        assert_eq!(
            parse_range("bytes=0-9223372036854775808", 10),
            Range::Part { start: 0, end: 9 }
        );
        assert_eq!(
            parse_range("bytes=0-99999999999999999999999", 10),
            Range::Full
        );

        let parent = std::env::temp_dir();
        let dir = parent.join(format!("lucina-serve-pure-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("manifest"), body()).unwrap();
        std::fs::write(dir.join("escape"), b"out").unwrap();
        std::os::unix::fs::symlink(dir.join("escape"), dir.join("via-in")).unwrap();
        std::os::unix::fs::symlink(
            parent.join("lucina-serve-pure-somewhere-else"),
            dir.join("out-link"),
        )
        .unwrap();
        let base = dir.canonicalize().unwrap();
        assert_eq!(resolve(&dir, "/manifest").unwrap(), base.join("manifest"));
        assert_eq!(resolve(&dir, "/sub/../manifest").unwrap_err().status, 403);
        assert_eq!(
            resolve(&dir, "/manifest?x=1").unwrap(),
            base.join("manifest")
        );
        assert_eq!(resolve(&dir, "/manifest#x").unwrap(), base.join("manifest"));
        assert_eq!(
            std::fs::read(resolve(&dir, "/manifest").unwrap()).unwrap(),
            body()
        );
        assert_eq!(resolve(&dir, "/via-in").unwrap(), base.join("escape"));
        assert_eq!(resolve(&dir, "manifest").unwrap_err().status, 400);
        assert_eq!(resolve(&dir, "http://h/manifest").unwrap_err().status, 400);
        assert_eq!(resolve(&dir, "/%zz").unwrap_err().status, 400);
        assert_eq!(resolve(&dir, "/a%00b").unwrap_err().status, 400);
        assert_eq!(resolve(&dir, "/a%0ab").unwrap_err().status, 400);
        assert_eq!(resolve(&dir, "/").unwrap_err().status, 404);
        assert_eq!(resolve(&dir, "/gone").unwrap_err().status, 404);
        assert_eq!(resolve(&dir, "/out-link").unwrap_err().status, 404);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
