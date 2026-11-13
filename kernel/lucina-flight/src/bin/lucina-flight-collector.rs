//! lucina-flight-collector: pulls the flight recorder's stream from Juno over SSH and keeps one folder per boot.
//!
//! ```text
//! lucina-flight-collector [--dir ~/scratch/juno-flight] [--host root@192.168.1.63] [--remote /sbin/lucina-flight]
//! ```
//! It runs `ssh <host> <remote>` (pinned host key, batch mode, keepalives every 5 s) and reads the program's standard output; when
//! the session ends or fails it tries again a second later. Nothing listens on this machine. A folder `<date>-boot<N>/` holds
//! `kmsg.log` (one line per kernel record: `<received unix ms> <seq> <level> <usec> <text>`), `events.log` (connections, gaps,
//! the end of the stream) and `meta.txt` (the boot id, the build, the status and the time of the last line received). A stream
//! that ends without a `Z` line and without the kernel saying it is turning the machine off is marked `stream ended without a
//! clean shutdown line`. Design: docs/design/juno-logging.md.

use lucina_flight::{Cursor, Frame, Hello, Taken, decode, decode_hello, is_shutdown_line};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// `YYYY-MM-DD` (UTC) of a unix time in ms, by the civil-from-days algorithm.
fn date_of(ms: u64) -> String {
    let z = (ms / 86_400_000) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    format!("{:04}-{:02}-{:02}", if m <= 2 { y + 1 } else { y }, m, d)
}

fn iso(ms: u64) -> String {
    let s = ms / 1000;
    format!(
        "{}T{:02}:{:02}:{:02}Z",
        date_of(ms),
        s % 86_400 / 3600,
        s % 3600 / 60,
        s % 60
    )
}

struct Boot {
    id: String,
    dir: PathBuf,
    n: u64,
    build: String,
    mode: String,
    juno_wall_ms: u64,
    juno_uptime_ms: u64,
    first_ms: u64,
    last_line_ms: u64,
    last_seq: Option<u64>,
    last_usec: u64,
    last_heartbeat_ms: Option<u64>,
    lines: u64,
    gaps: u64,
    connections: u64,
    shutdown_line: bool,
    /// "streaming", "clean", or the reason it was not.
    status: String,
    cursor: Cursor,
    kmsg: Option<File>,
    meta_written_ms: u64,
}

impl Boot {
    fn meta(&self) -> String {
        let mut s = String::new();
        let mut kv = |k: &str, v: String| {
            s.push_str(k);
            s.push('=');
            s.push_str(&v);
            s.push('\n');
        };
        kv("boot", self.id.clone());
        kv("n", self.n.to_string());
        kv("status", self.status.clone());
        kv("first_connect", iso(self.first_ms));
        kv("first_connect_ms", self.first_ms.to_string());
        kv(
            "last_line",
            if self.last_line_ms == 0 {
                "none".into()
            } else {
                iso(self.last_line_ms)
            },
        );
        kv("last_line_ms", self.last_line_ms.to_string());
        kv(
            "last_seq",
            self.last_seq.map_or("none".into(), |n| n.to_string()),
        );
        kv("last_kernel_usec", self.last_usec.to_string());
        kv(
            "last_heartbeat_ms",
            self.last_heartbeat_ms
                .map_or("none".into(), |n| n.to_string()),
        );
        kv("kernel_lines", self.lines.to_string());
        kv("gaps", self.gaps.to_string());
        kv("connections", self.connections.to_string());
        kv("shutdown_line_seen", self.shutdown_line.to_string());
        kv("mode", self.mode.clone());
        kv("juno_wall_ms_at_hello", self.juno_wall_ms.to_string());
        kv("juno_uptime_ms_at_hello", self.juno_uptime_ms.to_string());
        kv("build", self.build.clone());
        s
    }

    fn save_meta(&mut self) {
        let tmp = self.dir.join("meta.txt.tmp");
        if fs::write(&tmp, self.meta()).is_ok() {
            let _ = fs::rename(&tmp, self.dir.join("meta.txt"));
        }
        self.meta_written_ms = now_ms();
    }

    fn event(&self, text: &str) {
        if let Ok(mut f) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join("events.log"))
        {
            let _ = writeln!(f, "{} {}", iso(now_ms()), text);
        }
    }

    fn end(&mut self, why: &str) {
        if self.status != "streaming" {
            return;
        }
        let last = if self.last_line_ms == 0 {
            "no line received".to_string()
        } else {
            format!("last line received at {}", iso(self.last_line_ms))
        };
        self.status = if self.shutdown_line {
            format!(
                "clean: the kernel said it was turning the machine off or restarting it ({why})"
            )
        } else {
            format!("stream ended without a clean shutdown line ({why}); {last}")
        };
        self.event(&format!("end: {}", self.status));
        self.save_meta();
    }
}

struct Registry {
    root: PathBuf,
    boots: HashMap<String, Boot>,
    next_n: u64,
}

fn parse_meta(dir: &Path) -> Option<HashMap<String, String>> {
    let t = fs::read_to_string(dir.join("meta.txt")).ok()?;
    Some(
        t.lines()
            .filter_map(|l| l.split_once('='))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    )
}

fn load(root: &Path) -> Registry {
    let mut boots = HashMap::new();
    let mut next_n = 1;
    if let Ok(rd) = fs::read_dir(root) {
        for e in rd.flatten() {
            let dir = e.path();
            let Some(m) = parse_meta(&dir) else { continue };
            let n: u64 = m.get("n").and_then(|v| v.parse().ok()).unwrap_or(0);
            next_n = next_n.max(n + 1);
            let num = |k: &str| m.get(k).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
            let last_seq = m.get("last_seq").and_then(|v| v.parse::<u64>().ok());
            let b = Boot {
                id: m.get("boot").cloned().unwrap_or_default(),
                dir,
                n,
                build: m.get("build").cloned().unwrap_or_default(),
                mode: m.get("mode").cloned().unwrap_or_default(),
                juno_wall_ms: num("juno_wall_ms_at_hello"),
                juno_uptime_ms: num("juno_uptime_ms_at_hello"),
                first_ms: num("first_connect_ms"),
                last_line_ms: num("last_line_ms"),
                last_seq,
                last_usec: num("last_kernel_usec"),
                last_heartbeat_ms: m.get("last_heartbeat_ms").and_then(|v| v.parse().ok()),
                lines: num("kernel_lines"),
                gaps: num("gaps"),
                connections: num("connections"),
                shutdown_line: m.get("shutdown_line_seen").is_some_and(|v| v == "true"),
                // A collector that was stopped did not see the end of whatever was streaming.
                status: m.get("status").cloned().unwrap_or_default(),
                cursor: Cursor::new(last_seq.map_or(0, |s| s.saturating_add(1))),
                kmsg: None,
                meta_written_ms: 0,
            };
            let mut b = b;
            if b.status == "streaming" {
                b.status = format!(
                    "stream ended without a clean shutdown line (the collector was restarted); last line received at {}",
                    iso(b.last_line_ms)
                );
                b.save_meta();
            }
            boots.insert(b.id.clone(), b);
        }
    }
    Registry {
        root: root.to_path_buf(),
        boots,
        next_n,
    }
}

fn register(reg: &mut Registry, hello: &Hello) -> (String, u64) {
    let now = now_ms();
    // A new boot ends every other stream still open: one machine, one boot at a time.
    let others: Vec<String> = reg
        .boots
        .iter()
        .filter(|(k, b)| **k != hello.boot && b.status == "streaming")
        .map(|(k, _)| k.clone())
        .collect();
    for k in others {
        if let Some(b) = reg.boots.get_mut(&k) {
            b.end("a new boot started");
        }
    }
    if !reg.boots.contains_key(&hello.boot) {
        let n = reg.next_n;
        reg.next_n += 1;
        let dir = reg.root.join(format!("{}-boot{}", date_of(now), n));
        let _ = fs::create_dir_all(&dir);
        reg.boots.insert(
            hello.boot.clone(),
            Boot {
                id: hello.boot.clone(),
                dir,
                n,
                build: hello.build.clone(),
                mode: hello.mode.word().into(),
                juno_wall_ms: hello.wall_ms,
                juno_uptime_ms: hello.uptime_ms,
                first_ms: now,
                last_line_ms: 0,
                last_seq: None,
                last_usec: 0,
                last_heartbeat_ms: None,
                lines: 0,
                gaps: 0,
                connections: 0,
                shutdown_line: false,
                status: "streaming".into(),
                cursor: Cursor::new(0),
                kmsg: None,
                meta_written_ms: 0,
            },
        );
    }
    let b = reg.boots.get_mut(&hello.boot).unwrap();
    b.connections += 1;
    b.status = "streaming".into();
    b.mode = hello.mode.word().into();
    b.kmsg = OpenOptions::new()
        .create(true)
        .append(true)
        .open(b.dir.join("kmsg.log"))
        .ok();
    let resume = b.cursor.expected();
    b.event(&format!(
        "connection {}, mode {}, juno uptime {} ms, resume at {resume}",
        b.connections,
        hello.mode.word(),
        hello.uptime_ms
    ));
    b.save_meta();
    eprintln!(
        "boot {} ({}) connected, resume at {resume}",
        b.n,
        b.dir.display()
    );
    (hello.boot.clone(), resume)
}

fn apply(b: &mut Boot, frame: Frame) {
    let now = now_ms();
    match frame {
        Frame::Kernel {
            seq,
            level,
            usec,
            text,
        } => {
            match b.cursor.take(seq) {
                Taken::Duplicate => return,
                Taken::Keep { lost } => {
                    if let Some((from, to)) = lost {
                        b.gaps += 1;
                        b.event(&format!("gap: records {from}..{to} never received"));
                    }
                }
            }
            if let Some(f) = b.kmsg.as_mut() {
                let _ = writeln!(f, "{now} {seq} {level} {usec} {text}");
            }
            if is_shutdown_line(&text) {
                b.shutdown_line = true;
            }
            b.lines += 1;
            b.last_line_ms = now;
            b.last_seq = Some(seq);
            b.last_usec = usec;
        }
        Frame::Gap { from, to } => {
            b.gaps += 1;
            b.cursor.skip_to(to);
            b.event(&format!("gap reported by the laptop: records {from}..{to} were overwritten before they could be sent"));
            if let Some(f) = b.kmsg.as_mut() {
                let _ = writeln!(f, "{now} - - - [{from}..{to} lost before sending]");
            }
        }
        Frame::Heartbeat { .. } => {
            b.last_heartbeat_ms = Some(now);
            b.last_line_ms = now;
        }
        Frame::Clean { reason } => {
            b.shutdown_line = true;
            b.event(&format!("clean end: {reason}"));
        }
    }
    // Kernel lines are on disk at once; the summary is refreshed at most every 2 s.
    if now.saturating_sub(b.meta_written_ms) > 2000 {
        b.save_meta();
    }
}

/// How to reach the laptop. The command is `ssh <options> <host> <remote command>`; its standard output is the stream.
struct Link {
    host: String,
    remote: String,
    hostkey_alias: String,
    known_hosts: PathBuf,
    /// Seconds between the sleeps that follow a failed or ended session.
    retry_s: u64,
}

impl Link {
    fn command(&self) -> Command {
        let mut c = Command::new("ssh");
        c.args([
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=3",
            "-o",
            "ServerAliveInterval=5",
            "-o",
            "ServerAliveCountMax=3",
            "-o",
            "LogLevel=ERROR",
        ])
        .arg("-o")
        .arg(format!("UserKnownHostsFile={}", self.known_hosts.display()))
        .arg("-o")
        .arg(format!("HostKeyAlias={}", self.hostkey_alias))
        .arg(&self.host)
        .arg(&self.remote)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
        c
    }
}

/// One session: ssh, the greeting, the answer, then frames until the stream ends. Returns why it ended, and whether a greeting
/// came (a session that never got that far is not an event worth a line).
fn session(link: &Link, reg: &mut Registry) -> (String, bool) {
    let mut child = match link.command().spawn() {
        Ok(c) => c,
        Err(e) => return (format!("cannot run ssh: {e}"), false),
    };
    let (Some(mut stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        let _ = child.kill();
        return ("no pipes".into(), false);
    };
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    let greeted = reader.read_line(&mut line).is_ok_and(|n| n > 0);
    let Some(hello) = greeted.then(|| decode_hello(&line)).flatten() else {
        let _ = child.kill();
        let _ = child.wait();
        return ("no greeting".into(), false);
    };
    let (id, resume) = register(reg, &hello);
    if stdin
        .write_all(format!("RESUME {resume}\n").as_bytes())
        .and_then(|()| stdin.flush())
        .is_err()
    {
        finish(reg, &id, "could not answer the greeting");
        let _ = child.wait();
        return ("could not answer the greeting".into(), true);
    }
    let mut why = "the connection closed".to_string();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                let Some(frame) = decode(&line) else { continue };
                let clean = matches!(frame, Frame::Clean { .. });
                if let Some(b) = reg.boots.get_mut(&id) {
                    apply(b, frame);
                }
                if clean {
                    why = "the daemon said it was stopping".into();
                    break;
                }
            }
            Err(e) => {
                why = format!("read: {e}");
                break;
            }
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    finish(reg, &id, &why);
    (why, true)
}

fn finish(reg: &mut Registry, id: &str, why: &str) {
    if let Some(b) = reg.boots.get_mut(id) {
        b.end(why);
        eprintln!("boot {}: {}", b.n, b.status);
    }
}

fn main() {
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into()));
    let mut dir = home.join("scratch/juno-flight");
    let mut link = Link {
        host: "root@192.168.1.63".into(),
        remote: "/sbin/lucina-flight".into(),
        hostkey_alias: "192.168.1.47".into(),
        known_hosts: home.join("scratch/juno-ota/juno_known_hosts"),
        retry_s: 1,
    };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut v = || args.next().unwrap_or_default();
        match a.as_str() {
            "--dir" => dir = PathBuf::from(v()),
            "--host" => link.host = v(),
            "--remote" => link.remote = v(),
            "--hostkey-alias" => link.hostkey_alias = v(),
            "--known-hosts" => link.known_hosts = PathBuf::from(v()),
            "--retry" => link.retry_s = v().parse().unwrap_or(1).max(1),
            _ => {
                eprintln!(
                    "lucina-flight-collector [--dir DIR] [--host user@ip] [--remote CMD] [--hostkey-alias A] [--known-hosts FILE] [--retry SECONDS]"
                );
                std::process::exit(2);
            }
        }
    }
    let _ = fs::create_dir_all(&dir);
    let mut reg = load(&dir);
    eprintln!(
        "lucina-flight-collector: pulling from {} over ssh ({}), folders in {}",
        link.host,
        link.remote,
        dir.display()
    );
    let mut quiet = false;
    loop {
        let t = std::time::Instant::now();
        let (why, greeted) = session(&link, &mut reg);
        if greeted || !quiet {
            eprintln!("session ended: {why}");
        }
        // One line when the laptop first stays away; nothing per try after that.
        quiet = !greeted;
        // A session that ran for a while retries at once; one that failed waits, so a laptop that is off costs one ssh try a second.
        let wait = if t.elapsed() > Duration::from_secs(10) {
            0
        } else {
            link.retry_s
        };
        std::thread::sleep(Duration::from_secs(wait));
    }
}
