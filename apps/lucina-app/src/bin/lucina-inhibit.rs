//! lucina-inhibit: keep Juno from going to sleep (or its screen off) while a command runs, as `systemd-inhibit` does for logind.
//! The hold is a connection to the shell's control socket (`/run/lucina/shell.sock`, `{"op":"inhibit"}`) that stays open while
//! the command runs: when this program ends, by exit, a signal, SIGKILL or a dropped SSH session, the connection closes and the
//! shell drops the hold. Behaviour after systemd-inhibit; no systemd code.
//!
//! ```text
//! lucina-inhibit [--what=sleep|idle] [--who=NAME] [--why=TEXT] [--] COMMAND [ARG...]
//! lucina-inhibit --list
//! ```
//!
//! `sleep` (the default) keeps the machine awake and out of standby; the screen may still go off. `idle` keeps the screen on
//! too. The defaults are systemd-inhibit's: `--who` the command line, `--why` "Unknown reason". The exit status is the
//! command's, or 128 + its signal; 127 when it cannot be started; 1 when the shell refuses the hold; 2 for a bad command line.
//! `LUCINA    let stream = UnixStream::connect(path)SHELL_SOCK` names another socket (tests).

// Juno with no C library: Eyra is the libc and the program start, in Rust (docs/research/juno-pure-rust.md).
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"))]
extern crate eyra;

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::ExitStatusExt;
use std::process::{Command, ExitCode};

use serde_json::{Value, json};

const SOCKET: &str = "/run/lucina/shell.sock";

#[derive(Debug, PartialEq, Eq)]
enum Opts {
    List,
    Run { what: String, who: String, why: String, command: Vec<String> },
}

/// The command line, or why it is not one.
fn parse(args: &[String]) -> Result<Opts, String> {
    let (mut what, mut who, mut why) = ("sleep".to_string(), None, None);
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "--" {
            i += 1;
            break;
        }
        if a == "--list" {
            return Ok(Opts::List);
        }
        let Some(opt) = a.strip_prefix("--") else {
            break;
        };
        let (name, value) = match opt.split_once('=') {
            Some((n, v)) => (n, v.to_string()),
            None => {
                i += 1;
                (opt, args.get(i).ok_or_else(|| format!("--{opt} needs a value"))?.clone())
            }
        };
        match name {
            "what" => match value.as_str() {
                "sleep" | "idle" | "idle:sleep" | "sleep:idle" => what = value,
                _ => return Err("--what must be sleep or idle".into()),
            },
            "who" => who = Some(value),
            "why" => why = Some(value),
            _ => return Err(format!("unknown option --{name}")),
        }
        i += 1;
    }
    let command: Vec<String> = args[i..].to_vec();
    if command.is_empty() {
        return Err("no command to run".into());
    }
    let who = who.unwrap_or_else(|| command.join(" ").chars().take(64).collect());
    Ok(Opts::Run { what, who, why: why.unwrap_or_else(|| "Unknown reason".into()), command })
}

/// Sends one request line and reads one reply line.
fn ask(stream: &UnixStream, request: &Value) -> Result<Value, String> {
    let mut line = request.to_string();
    line.push('\n');
    (&*stream).write_all(line.as_bytes()).map_err(|e| e.to_string())?;
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply).map_err(|e| e.to_string())?;
    serde_json::from_str(reply.trim()).map_err(|_| format!("not an answer: {reply:?}"))
}

/// "3m 20s", "45s", "2h 5m".
fn duration(s: u64) -> String {
    match s {
        s if s >= 3600 => format!("{}h {}m", s / 3600, s % 3600 / 60),
        s if s >= 60 => format!("{}m {}s", s / 60, s % 60),
        s => format!("{s}s"),
    }
}

fn run(args: &[String], path: &str) -> Result<u8, (u8, String)> {
    let opts = parse(args).map_err(|e| (2, format!("{e}\nusage: lucina-inhibit [--what=sleep|idle] [--who=NAME] [--why=TEXT] [--] COMMAND [ARG...] | --list")))?;
    let stream = UnixStream::connect(path).map_err(|e| (1, format!("{path}: {e}")))?;
    match opts {
        Opts::List => {
            let v = ask(&stream, &json!({"op": "holds"})).map_err(|e| (1, e))?;
            if v["ok"] != true {
                return Err((1, v["error"].as_str().unwrap_or("refused").to_string()));
            }
            println!("WHO\tWHAT\tWHY\tFOR");
            for h in v["inhibitors"].as_array().into_iter().flatten() {
                println!(
                    "{}\t{}\t{}\t{}",
                    h["who"].as_str().unwrap_or(""),
                    h["what"].as_str().unwrap_or(""),
                    h["why"].as_str().unwrap_or(""),
                    duration(h["for_s"].as_u64().unwrap_or(0))
                );
            }
            Ok(0)
        }
        Opts::Run { what, who, why, command } => {
            let v = ask(&stream, &json!({"op": "inhibit", "what": what, "who": who, "why": why})).map_err(|e| (1, e))?;
            if v["ok"] != true {
                return Err((1, v["error"].as_str().unwrap_or("refused").to_string()));
            }
            // std's sockets are close-on-exec: the command does not inherit the hold, this program keeps it.
            let status = Command::new(&command[0])
                .args(&command[1..])
                .status()
                .map_err(|e| (127, format!("{}: {e}", command[0])))?;
            drop(stream);
            Ok(match (status.code(), status.signal()) {
                (Some(c), _) => c as u8,
                (None, Some(sig)) => (128 + sig) as u8,
                (None, None) => 1,
            })
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = std::env::var("LUCINA_SHELL_SOCK").unwrap_or_else(|_| SOCKET.into());
    match run(&args, &path) {
        Ok(code) => ExitCode::from(code),
        Err((code, why)) => {
            eprintln!("lucina-inhibit: {why}");
            ExitCode::from(code)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn defaults_are_systemd_inhibits() {
        assert_eq!(
            parse(&args(&["sleep", "30"])),
            Ok(Opts::Run { what: "sleep".into(), who: "sleep 30".into(), why: "Unknown reason".into(), command: args(&["sleep", "30"]) })
        );
    }

    #[test]
    fn both_option_forms_and_the_double_dash() {
        let want = Opts::Run { what: "idle".into(), who: "agent".into(), why: "a test".into(), command: args(&["--x", "y"]) };
        assert_eq!(parse(&args(&["--what=idle", "--who=agent", "--why=a test", "--", "--x", "y"])), Ok(want));
        let want = Opts::Run { what: "idle".into(), who: "agent".into(), why: "a test".into(), command: args(&["true"]) };
        assert_eq!(parse(&args(&["--what", "idle", "--who", "agent", "--why", "a test", "true"])), Ok(want));
        assert_eq!(parse(&args(&["--list"])), Ok(Opts::List));
    }

    #[test]
    fn bad_command_lines() {
        assert!(parse(&args(&[])).is_err());
        assert!(parse(&args(&["--who=x"])).is_err());
        assert!(parse(&args(&["--what=screen", "true"])).is_err());
        assert!(parse(&args(&["--when=now", "true"])).is_err());
        assert!(parse(&args(&["--who"])).is_err());
    }

    #[test]
    fn durations_read_as_written() {
        assert_eq!(duration(45), "45s");
        assert_eq!(duration(200), "3m 20s");
        assert_eq!(duration(7500), "2h 5m");
    }

    /// The whole program against a socket that answers as the shell's does: the hold is held while the command runs and
    /// ends with this program.
    #[test]
    fn a_hold_for_the_run_of_the_command() {
        use std::os::unix::net::UnixListener;
        let path = std::env::temp_dir().join(format!("lucina-inhibit-test-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let (s, _) = listener.accept().unwrap();
            let mut line = String::new();
            let mut r = BufReader::new(&s);
            r.read_line(&mut line).unwrap();
            (&s).write_all(b"{\"ok\":true,\"id\":7}\n").unwrap();
            let mut rest = String::new();
            // The connection stays open until the program is done: then the read sees the end.
            let n = r.read_line(&mut rest).unwrap();
            (line, n)
        });
        let code = run(&args(&["--who=t", "--why=w", "sh", "-c", "exit 3"]), path.to_str().unwrap()).unwrap();
        assert_eq!(code, 3);
        let (line, n) = server.join().unwrap();
        let v: Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(v, json!({"op": "inhibit", "what": "sleep", "who": "t", "why": "w"}));
        assert_eq!(n, 0);
        let _ = std::fs::remove_file(&path);
    }
}
