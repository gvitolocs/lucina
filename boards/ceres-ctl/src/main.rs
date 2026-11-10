//! ceres-ctl: the buildhost side of Ceres. Sends one command line to the ESP32
//! over USB serial (CH340, 115200 8N1) and prints the board's reply line.
//! When invoked as `dsi-power`, maps its verbs onto ceres-ctl commands.

use std::env;
use std::fs;
use std::io::{Read, Write};
use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

const DEFAULT_TIMEOUT_S: u64 = 25;
const READY_TIMEOUT: Duration = Duration::from_millis(1500);
const RESEND_AFTER: Duration = Duration::from_secs(3);
const POLL: Duration = Duration::from_millis(50);

/// How a reply line from the board is classified.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Reply {
    /// Line starts with `ok`.
    Ok(String),
    /// Line starts with `err`.
    Err(String),
    /// Anything else (ready line, empty, noise): keep reading.
    Other,
}

fn classify_reply(line: &str) -> Reply {
    let line = line.trim_end_matches(['\r', '\n']);
    if line.is_empty() {
        return Reply::Other;
    }
    if let Some(rest) = line.strip_prefix("ok") {
        if rest.is_empty() || rest.starts_with(' ') {
            return Reply::Ok(line.to_string());
        }
    }
    if let Some(rest) = line.strip_prefix("err") {
        if rest.is_empty() || rest.starts_with(' ') {
            return Reply::Err(line.to_string());
        }
    }
    Reply::Other
}

/// Maps a `dsi-power` invocation to the equivalent ceres-ctl command words.
/// Returns None when the verb has no mapping.
fn dsi_power_map(words: &[String]) -> Option<Vec<String>> {
    let verb = words.first()?.as_str();
    let mapped: &[&str] = match verb {
        "cycle" => &["cycle"],
        "off" => &["press", "6000"],
        "on" => &["press", "300"],
        "status" => &["status"],
        _ => return None,
    };
    Some(mapped.iter().map(|s| s.to_string()).collect())
}

/// Commands that may be sent a second time if the board stays silent.
fn is_resendable(command: &[String]) -> bool {
    matches!(
        command.first().map(|s| s.as_str()),
        Some("status") | Some("ver") | Some("help") | Some("release")
    )
}

struct Args {
    port: Option<String>,
    timeout: Duration,
    command: Vec<String>,
}

fn parse_args(argv: Vec<String>) -> Result<Args, String> {
    let mut port = None;
    let mut timeout = Duration::from_secs(DEFAULT_TIMEOUT_S);
    let mut words: Vec<String> = Vec::new();
    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--port" => {
                i += 1;
                port = Some(
                    argv.get(i)
                        .ok_or("--port needs a path")?
                        .clone(),
                );
            }
            "--timeout" => {
                i += 1;
                let s: u64 = argv
                    .get(i)
                    .ok_or("--timeout needs seconds")?
                    .parse()
                    .map_err(|_| "--timeout needs a number of seconds")?;
                timeout = Duration::from_secs(s);
            }
            other => words.push(other.to_string()),
        }
        i += 1;
    }
    if words.is_empty() {
        return Err("no command given (try: status, press <ms>, cycle, release, ver, help, reboot)".into());
    }
    Ok(Args {
        port,
        timeout,
        command: words,
    })
}

/// First entry of /dev/serial/by-id/ whose name contains `1a86` (the CH340).
fn find_ch340_port() -> Option<String> {
    let dir = fs::read_dir("/dev/serial/by-id").ok()?;
    let mut names: Vec<String> = dir
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains("1a86"))
        .collect();
    names.sort();
    names
        .first()
        .map(|n| format!("/dev/serial/by-id/{n}"))
}

/// Reads lines from the port until `deadline`, returning the first line that
/// classifies as Ok or Err. Sends `command` again after RESEND_AFTER of
/// silence when the command is resendable.
fn run_session(
    port: &mut dyn serialport::SerialPort,
    command: &[String],
    timeout: Duration,
) -> Result<Reply, String> {
    let line = command.join(" ");
    let start = Instant::now();
    let mut resent = false;
    let mut buf: Vec<u8> = Vec::new();
    loop {
        let mut chunk = [0u8; 256];
        match port.read(&mut chunk) {
            Ok(n) if n > 0 => buf.extend_from_slice(&chunk[..n]),
            Ok(_) => {}
            Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => return Err(format!("read error: {e}")),
        }
        while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
            let raw: Vec<u8> = buf.drain(..=pos).collect();
            let text = String::from_utf8_lossy(&raw);
            match classify_reply(&text) {
                Reply::Other => continue,
                r => return Ok(r),
            }
        }
        if start.elapsed() >= timeout {
            return Err("timeout waiting for the board".into());
        }
        if !resent
            && is_resendable(command)
            && start.elapsed() >= RESEND_AFTER
        {
            resent = true;
            port.write_all(line.as_bytes())
                .and_then(|_| port.write_all(b"\n"))
                .and_then(|_| port.flush())
                .map_err(|e| format!("write error: {e}"))?;
        }
        std::thread::sleep(POLL);
    }
}

fn main() -> ExitCode {
    let argv: Vec<String> = env::args().skip(1).collect();
    let prog = env::args()
        .next()
        .and_then(|p| Path::new(&p).file_name().map(|s| s.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "ceres-ctl".into());

    let mut args = match parse_args(argv) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("ceres-ctl: {e}");
            return ExitCode::from(2);
        }
    };
    if prog == "dsi-power" {
        match dsi_power_map(&args.command) {
            Some(mapped) => args.command = mapped,
            None => {
                eprintln!("dsi-power: unknown command (try: on, off, cycle, status)");
                return ExitCode::from(2);
            }
        }
    }

    let port_path = match args.port.clone().or_else(find_ch340_port) {
        Some(p) => p,
        None => {
            eprintln!(
                "ceres-ctl: no CH340 found in /dev/serial/by-id (looked for 1a86); pass --port <path>"
            );
            return ExitCode::from(2);
        }
    };

    let mut port = match serialport::new(&port_path, 115200)
        .data_bits(serialport::DataBits::Eight)
        .parity(serialport::Parity::None)
        .stop_bits(serialport::StopBits::One)
        .timeout(POLL)
        .open()
    {
        Ok(p) => p,
        Err(e) => {
            eprintln!("ceres-ctl: cannot open {port_path}: {e}");
            return ExitCode::from(2);
        }
    };
    // DTR and RTS stay as opening left them (both asserted). The DevKit's auto-reset pulls EN low while RTS is
    // asserted and DTR is not, and IO0 low (download mode) in the opposite state: clearing DTR before RTS, as v0
    // first did, reset the board on every call (status showed up_ms=10) and would cut a running press.

    // Opening may reset the board; give it up to 1.5 s to say it is ready.
    let ready_deadline = Instant::now() + READY_TIMEOUT;
    let mut buf: Vec<u8> = Vec::new();
    while Instant::now() < ready_deadline {
        let mut chunk = [0u8; 256];
        match port.read(&mut chunk) {
            Ok(n) if n > 0 => {
                buf.extend_from_slice(&chunk[..n]);
                if let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                    let raw: Vec<u8> = buf.drain(..=pos).collect();
                    if String::from_utf8_lossy(&raw).starts_with("ceres ready") {
                        break;
                    }
                }
            }
            Ok(_) => {}
            Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => {
                eprintln!("ceres-ctl: read error: {e}");
                return ExitCode::from(2);
            }
        }
    }

    let command_line = args.command.join(" ");
    if let Err(e) = port
        .write_all(command_line.as_bytes())
        .and_then(|_| port.write_all(b"\n"))
        .and_then(|_| port.flush())
    {
        eprintln!("ceres-ctl: write error: {e}");
        return ExitCode::from(2);
    }

    match run_session(&mut *port, &args.command, args.timeout) {
        Ok(Reply::Ok(line)) | Ok(Reply::Err(line)) => {
            println!("{line}");
            if matches!(classify_reply(&line), Reply::Ok(_)) {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            }
        }
        Ok(Reply::Other) => unreachable!("run_session only returns Ok/Err"),
        Err(e) => {
            eprintln!("ceres-ctl: {e}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn dsi_power_maps_verbs() {
        assert_eq!(dsi_power_map(&words(&["cycle"])), Some(words(&["cycle"])));
        assert_eq!(
            dsi_power_map(&words(&["off"])),
            Some(words(&["press", "6000"]))
        );
        assert_eq!(
            dsi_power_map(&words(&["on"])),
            Some(words(&["press", "300"]))
        );
        assert_eq!(
            dsi_power_map(&words(&["status"])),
            Some(words(&["status"]))
        );
        assert_eq!(dsi_power_map(&words(&["reboot"])), None);
        assert_eq!(dsi_power_map(&words(&[])), None);
    }

    #[test]
    fn parse_args_flags_and_command() {
        let a = parse_args(words(&["--port", "/dev/ttyUSB9", "--timeout", "5", "press", "500"])).unwrap();
        assert_eq!(a.port.as_deref(), Some("/dev/ttyUSB9"));
        assert_eq!(a.timeout, Duration::from_secs(5));
        assert_eq!(a.command, words(&["press", "500"]));

        let a = parse_args(words(&["status"])).unwrap();
        assert_eq!(a.port, None);
        assert_eq!(a.timeout, Duration::from_secs(DEFAULT_TIMEOUT_S));
        assert_eq!(a.command, words(&["status"]));

        assert!(parse_args(words(&[])).is_err());
        assert!(parse_args(words(&["--port"])).is_err());
        assert!(parse_args(words(&["--timeout", "soon", "status"])).is_err());
    }

    #[test]
    fn classify_ok_and_err() {
        assert_eq!(
            classify_reply("ok pressed 500\n"),
            Reply::Ok("ok pressed 500".into())
        );
        assert_eq!(
            classify_reply("ok\r\n"),
            Reply::Ok("ok".into())
        );
        assert_eq!(
            classify_reply("err busy\n"),
            Reply::Err("err busy".into())
        );
        assert_eq!(
            classify_reply("err unknown command"),
            Reply::Err("err unknown command".into())
        );
    }

    #[test]
    fn classify_skips_noise() {
        assert_eq!(classify_reply("ceres ready v0.1 board=esp32\n"), Reply::Other);
        assert_eq!(classify_reply("\n"), Reply::Other);
        assert_eq!(classify_reply("   \r\n"), Reply::Other);
        // "okay" and "error" are not replies.
        assert_eq!(classify_reply("okay\n"), Reply::Other);
        assert_eq!(classify_reply("error\n"), Reply::Other);
    }

    #[test]
    fn resendable_commands() {
        assert!(is_resendable(&words(&["status"])));
        assert!(is_resendable(&words(&["ver"])));
        assert!(is_resendable(&words(&["help"])));
        assert!(is_resendable(&words(&["release"])));
        assert!(!is_resendable(&words(&["press", "500"])));
        assert!(!is_resendable(&words(&["cycle"])));
        assert!(!is_resendable(&words(&["reboot"])));
    }
}
