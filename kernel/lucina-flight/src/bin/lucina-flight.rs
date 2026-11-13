//! lucina-flight: Juno's flight recorder, the laptop's half. It follows /dev/kmsg and writes every record to its standard output the
//! moment it is produced. Nothing on the laptop runs it or listens for it: the collector on buildhost (`lucina-flight-collector`)
//! opens an SSH session to the laptop and runs this program in it, so the stream is authenticated and encrypted, no port is
//! open on either machine, and the retry loop lives on buildhost, where a wake costs nothing. With no collector connected this
//! program does not exist and the idle cost is zero.
//!
//! ```text
//! lucina-flight      greets on stdout, reads `RESUME <seq>` on stdin, then streams (the format is in lib.rs)
//! ```
//! `lucina.flight=off|on|trace` on the kernel command line (or the word in `/data/lucina/flight`, `/etc/lucina/flight`):
//! `off` answers the collector with a `Z off` and ends; `trace` adds a heartbeat every 0.5 s (two wakes a second: for the reset
//! hunt only). Every line goes through the scrubber first. Design: docs/design/juno-logging.md.

// Juno with no C library: Eyra is the libc and the program start, in Rust (docs/research/juno-pure-rust.md).
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"))]
extern crate eyra;

use lucina_flight::{
    Cursor, Frame, Hello, Mode, Taken, cmdline_value, encode, encode_hello, parse_kmsg,
    scrub::scrub,
};
use std::fs;
use std::io::{self, BufRead, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_term(_: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

fn install_signals() {
    // SAFETY: sigaction with a handler that only stores to an atomic. No SA_RESTART, so that a blocked read of /dev/kmsg
    // returns EINTR and the loop sees the flag. SIGPIPE is ignored: a collector that went away shows as a failed write.
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = on_term as extern "C" fn(libc::c_int) as usize;
        sa.sa_flags = 0;
        libc::sigemptyset(&mut sa.sa_mask);
        libc::sigaction(libc::SIGTERM, &sa, std::ptr::null_mut());
        libc::sigaction(libc::SIGINT, &sa, std::ptr::null_mut());
        libc::sigaction(libc::SIGHUP, &sa, std::ptr::null_mut());
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
}

fn read_first(paths: &[&str]) -> Option<String> {
    paths
        .iter()
        .find_map(|p| fs::read_to_string(p).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn mode_setting() -> Mode {
    let cmdline = fs::read_to_string("/proc/cmdline").unwrap_or_default();
    let word = cmdline_value(&cmdline, "lucina.flight")
        .map(str::to_string)
        .or_else(|| read_first(&["/data/lucina/flight", "/etc/lucina/flight"]));
    word.as_deref().and_then(Mode::parse).unwrap_or(Mode::On)
}

fn uptime_ms() -> u64 {
    fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|t| {
            t.split_whitespace()
                .next()
                .and_then(|s| s.parse::<f64>().ok())
        })
        .map_or(0, |s| (s * 1000.0) as u64)
}

fn wall_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// One id per boot: made at the first run from the system's random source, kept in /run (RAM) so that every later run of the same
/// boot sends the same one.
fn boot_id() -> String {
    let path = "/run/lucina/flight.boot";
    if let Ok(s) = fs::read_to_string(path) {
        let s = s.trim().to_string();
        if s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit()) {
            return s;
        }
    }
    let mut raw = [0u8; 16];
    let ok = fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut raw))
        .is_ok();
    if !ok {
        // No random source: the clock and the uptime still make two boots differ.
        let t = wall_ms() ^ (uptime_ms() << 20);
        raw[..8].copy_from_slice(&t.to_le_bytes());
    }
    let id: String = raw.iter().map(|b| format!("{b:02x}")).collect();
    let _ = fs::create_dir_all("/run/lucina");
    let _ = fs::write(path, &id);
    id
}

fn build_text() -> String {
    fs::read_to_string("/proc/version")
        .unwrap_or_default()
        .trim()
        .chars()
        .take(200)
        .collect()
}

type Out = Arc<Mutex<io::Stdout>>;

fn send(out: &Out, frame: &Frame) -> io::Result<()> {
    let line = encode(frame);
    let mut o = out.lock().map_err(|_| io::Error::other("poisoned"))?;
    o.write_all(line.as_bytes())?;
    o.flush()
}

fn run() -> Result<(), String> {
    install_signals();
    let mode = mode_setting();
    let out: Out = Arc::new(Mutex::new(io::stdout()));
    let hello = Hello {
        boot: boot_id(),
        mode,
        uptime_ms: uptime_ms(),
        wall_ms: wall_ms(),
        build: build_text(),
    };
    {
        let mut o = out.lock().map_err(|_| "poisoned".to_string())?;
        o.write_all(encode_hello(&hello).as_bytes())
            .and_then(|()| o.flush())
            .map_err(|e| format!("hello: {e}"))?;
    }
    let mut line = String::new();
    io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| format!("resume: {e}"))?;
    let resume: u64 = line
        .trim()
        .strip_prefix("RESUME ")
        .and_then(|n| n.parse().ok())
        .ok_or_else(|| format!("bad answer {:?}", line.trim()))?;
    if mode == Mode::Off {
        let _ = send(
            &out,
            &Frame::Clean {
                reason: "off (lucina.flight=off)".into(),
            },
        );
        return Ok(());
    }

    // The heartbeat of `trace`: a thread of its own so that a quiet log still shows life. Off in `on`.
    let alive = Arc::new(AtomicBool::new(true));
    if mode == Mode::Trace {
        let (alive, out) = (alive.clone(), out.clone());
        std::thread::spawn(move || {
            while alive.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(500));
                if send(
                    &out,
                    &Frame::Heartbeat {
                        uptime_ms: uptime_ms(),
                    },
                )
                .is_err()
                {
                    STOP.store(true, Ordering::SeqCst);
                    break;
                }
            }
        });
    }
    let r = stream_log(&out, resume);
    alive.store(false, Ordering::SeqCst);
    r
}

fn stream_log(out: &Out, resume: u64) -> Result<(), String> {
    let mut kmsg = fs::File::open("/dev/kmsg").map_err(|e| format!("open /dev/kmsg: {e}"))?;
    let mut cursor = Cursor::new(resume);
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        if STOP.load(Ordering::SeqCst) {
            let _ = send(
                out,
                &Frame::Clean {
                    reason: "stopped".into(),
                },
            );
            return Ok(());
        }
        let n = match kmsg.read(&mut buf) {
            Ok(0) => {
                std::thread::sleep(Duration::from_secs(1));
                continue;
            }
            Ok(n) => n,
            // Overtaken by the ring: the next record's number shows the gap.
            Err(e) if e.raw_os_error() == Some(libc::EPIPE) => continue,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(format!("read /dev/kmsg: {e}")),
        };
        let text = String::from_utf8_lossy(&buf[..n]);
        for l in text.split('\n') {
            if l.is_empty() || l.starts_with(' ') {
                continue;
            }
            let Some(k) = parse_kmsg(l) else { continue };
            match cursor.take(k.seq) {
                Taken::Duplicate => continue,
                Taken::Keep { lost } => {
                    if let Some((from, to)) = lost {
                        send(out, &Frame::Gap { from, to }).map_err(|e| format!("send: {e}"))?;
                    }
                }
            }
            let frame = Frame::Kernel {
                seq: k.seq,
                level: k.level,
                usec: k.usec,
                text: scrub(k.text),
            };
            send(out, &frame).map_err(|e| format!("send: {e}"))?;
        }
    }
}

fn main() {
    if std::env::args().skip(1).any(|a| a == "-h" || a == "--help") {
        println!(
            "lucina-flight   stream /dev/kmsg on stdout for the flight collector (docs/design/juno-logging.md)"
        );
        return;
    }
    if let Err(e) = run() {
        eprintln!("lucina-flight: {e}");
        std::process::exit(1);
    }
}
