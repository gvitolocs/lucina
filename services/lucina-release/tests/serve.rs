//! The `serve` subcommand as a process: a temp folder on 127.0.0.1, one real request, and the argument checks.
//! The behaviour of the server itself is `src/serve.rs`'s own tests; these cover the wiring.
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_lucina-release");

fn plain(args: &[&str]) -> (bool, String) {
    let o = Command::new(BIN).args(args).output().unwrap();
    (
        o.status.success(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

/// Starts the server on a port the OS picks, and returns it with that port and the reader of its stderr.
fn start(dir: &str) -> (Child, String, BufReader<std::process::ChildStderr>) {
    let stop = |mut child: Child| {
        let _ = child.kill();
        let _ = child.wait();
    };
    let mut child = Command::new(BIN)
        .args(["serve", dir, "--addr", "127.0.0.1:0"])
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut err = BufReader::new(child.stderr.take().unwrap());
    let mut line = String::new();
    if err.read_line(&mut line).unwrap_or(0) == 0 {
        stop(child);
        panic!("the server printed nothing");
    }
    let Some(addr) = line
        .rsplit_once("http://")
        .and_then(|r| r.1.strip_suffix("/\n"))
        .map(str::to_string)
    else {
        stop(child);
        panic!("no address in {line:?}");
    };
    // The listener is up when the line is printed, but give it a moment to be scheduled.
    for _ in 0..200 {
        if TcpStream::connect(&addr).is_ok() {
            return (child, addr, err);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    stop(child);
    panic!("nothing listening on {addr}");
}

fn ask(addr: &str, request: &str) -> (String, Vec<u8>) {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    s.write_all(request.as_bytes()).unwrap();
    let mut all = Vec::new();
    s.read_to_end(&mut all).unwrap();
    let i = all
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("no header end");
    (
        String::from_utf8_lossy(&all[..i]).into_owned(),
        all[i + 4..].to_vec(),
    )
}

#[test]
fn serves_and_refuses() {
    let dir = std::env::temp_dir().join(format!("lucina-serve-bin-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let want: Vec<u8> = (0..1000u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(dir.join("manifest"), &want).unwrap();

    let (mut child, addr, _err) = start(dir.to_str().unwrap());
    let (h, b) = ask(&addr, "GET /manifest HTTP/1.1\r\n\r\n");
    assert!(h.starts_with("HTTP/1.1 200 OK"), "{h}");
    assert_eq!(b, want);
    let (h, b) = ask(&addr, "GET /manifest HTTP/1.1\r\nRange: bytes=500-\r\n\r\n");
    assert!(h.starts_with("HTTP/1.1 206 "), "{h}");
    assert!(h.contains("\r\nContent-Range: bytes 500-999/1000"), "{h}");
    assert_eq!(b, want[500..]);
    let (h, _) = ask(&addr, "GET /../etc/hostname HTTP/1.1\r\n\r\n");
    assert!(
        h.starts_with("HTTP/1.1 403 ") || h.starts_with("HTTP/1.1 404 "),
        "{h}"
    );
    let (h, _) = ask(&addr, "GET /gone HTTP/1.1\r\n\r\n");
    assert!(h.starts_with("HTTP/1.1 404 "), "{h}");
    assert!(child.kill().is_ok());
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn one_log_line_per_request() {
    let dir = std::env::temp_dir().join(format!("lucina-serve-bin-log-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("manifest"), b"hello").unwrap();

    let (mut child, addr, mut err) = start(dir.to_str().unwrap());
    ask(&addr, "GET /manifest HTTP/1.1\r\n\r\n");
    ask(&addr, "GET /gone HTTP/1.1\r\n\r\n");
    let mut lines = Vec::new();
    let until = Instant::now() + Duration::from_secs(10);
    while lines.len() < 2 && Instant::now() < until {
        let mut l = String::new();
        if err.read_line(&mut l).unwrap_or(0) == 0 {
            break;
        }
        lines.push(l);
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(lines.len(), 2, "lines: {lines:?}");
    assert!(
        lines[0].starts_with("serve: 127.0.0.1:") && lines[0].contains("GET /manifest 200 5"),
        "{:?}",
        lines[0]
    );
    assert!(lines[1].contains("GET /gone 404"), "{:?}", lines[1]);
    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn bad_arguments() {
    let (ok, e) = plain(&["serve", "/tmp"]);
    assert!(!ok && e.contains("--addr is required"), "{e}");
    let (ok, e) = plain(&["serve", "/tmp", "--addr", "not-an-address"]);
    assert!(!ok && e.contains("--addr is not an <ip:port>"), "{e}");
    let (ok, e) = plain(&["serve", "/tmp/no-such-folder-here", "--addr", "127.0.0.1:0"]);
    assert!(!ok && e.contains("is not a folder"), "{e}");
    let (ok, e) = plain(&["serve", "/tmp", "/extra", "--addr", "127.0.0.1:0"]);
    assert!(!ok && e.contains("exactly one folder"), "{e}");
}
