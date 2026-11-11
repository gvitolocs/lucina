//! `bash`: one `/bin/sh -c` command (BusyBox's, as the shell's terminal has it) as the signed-in user, which on Juno is root
//! (there is one user): a clean environment, in the owner's home, its own process group so that the timeout kills whatever it
//! started. The off-limits names are refused before it runs (`offlimits::check_command`, in `tools`).
use std::io::Read;
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

pub const MAX_OUTPUT: usize = 64 << 10;
pub const MAX_TIMEOUT_SECS: u64 = 110;
pub const MAX_COMMAND: usize = 16 << 10;
pub const SHELL: &str = "/bin/sh";

/// What a run needs; the tests use their own shell.
pub struct Run<'a> {
    pub shell: &'a str,
    pub command: &'a str,
    pub cwd: &'a Path,
    pub timeout: Duration,
}

pub struct Output {
    pub text: String,
    pub truncated: bool,
    pub code: Option<i32>,
    pub signal: Option<i32>,
    pub timed_out: bool,
    pub ms: u64,
}

pub fn run(r: &Run) -> Result<Output, String> {
    let mut pipe = [-1; 2];
    // SAFETY: pipe2 fills the two-element array.
    if unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC) } < 0 {
        return Err(format!("no pipe: {}", std::io::Error::last_os_error()));
    }
    // SAFETY: both were just made and have one owner each.
    let (read_end, write_end) =
        unsafe { (OwnedFd::from_raw_fd(pipe[0]), OwnedFd::from_raw_fd(pipe[1])) };
    let home = r.cwd.display().to_string();
    let mut cmd = Command::new(r.shell);
    cmd.arg("-c")
        .arg(r.command)
        .current_dir(r.cwd)
        .env_clear()
        .env("PATH", "/bin:/usr/bin")
        .env("HOME", &home)
        .env("LANG", "C.UTF-8")
        .stdin(Stdio::null())
        .stdout(Stdio::from(
            write_end.try_clone().map_err(|e| e.to_string())?,
        ))
        .stderr(Stdio::from(write_end));
    // SAFETY: setpgid is async-signal-safe. Its own group: the timeout kills whatever it started.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setpgid(0, 0) != 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    let t0 = Instant::now();
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("{}: {}", r.shell, crate::places::plain(&e)))?;
    drop(cmd);
    let pid = child.id() as libc::pid_t;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut f = std::fs::File::from(read_end);
        let mut kept = Vec::new();
        let mut buf = [0u8; 8192];
        let mut truncated = false;
        // Read to the end even past the cap, so the command never blocks on a full pipe.
        while let Ok(n) = f.read(&mut buf) {
            if n == 0 {
                break;
            }
            let room = MAX_OUTPUT - kept.len();
            if n > room {
                truncated = true;
            }
            kept.extend_from_slice(&buf[..n.min(room)]);
        }
        let _ = tx.send((kept, truncated));
    });
    let deadline = t0 + r.timeout;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) => {}
            Err(_) => break None,
        }
        if Instant::now() >= deadline {
            timed_out = true;
            // SAFETY: a signal to the command's own group, and to it in case the group is not made yet.
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
                libc::kill(pid, libc::SIGKILL);
            }
            break child.wait().ok();
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    // What it left running in the background goes too, and with it the last writer of the pipe.
    // SAFETY: as above.
    unsafe { libc::kill(-pid, libc::SIGKILL) };
    let (kept, truncated) = rx.recv_timeout(Duration::from_secs(2)).unwrap_or_default();
    let mut text = String::from_utf8_lossy(&kept).into_owned();
    if truncated {
        text += &format!("\n[output cut at {} KiB]", MAX_OUTPUT >> 10);
    }
    Ok(Output {
        text,
        truncated,
        code: status.and_then(|s| s.code()),
        signal: status.and_then(|s| s.signal()),
        timed_out,
        ms: t0.elapsed().as_millis() as u64,
    })
}

impl Output {
    pub fn to_result(&self) -> Value {
        let how = match (self.timed_out, self.code, self.signal) {
            (true, _, _) => "timed out and was stopped".to_owned(),
            (_, Some(c), _) => format!("exit status {c}"),
            (_, _, Some(s)) => format!("ended by signal {s}"),
            _ => "ended".to_owned(),
        };
        json!({
            "text": format!("{}\n[{how}, {} ms]", self.text.trim_end(), self.ms),
            "output": self.text, "exitCode": self.code, "signal": self.signal, "timedOut": self.timed_out, "truncated": self.truncated,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(command: &str, secs: u64) -> Output {
        let dir = std::env::temp_dir();
        run(&Run {
            shell: "/bin/sh",
            command,
            cwd: &dir,
            timeout: Duration::from_secs(secs),
        })
        .unwrap()
    }

    #[test]
    fn output_and_errors_come_in_one_stream_with_the_status() {
        let o = sh("echo out; echo err >&2; exit 3", 5);
        assert_eq!(o.text, "out\nerr\n");
        assert_eq!(o.code, Some(3));
        assert!(!o.timed_out);
        assert!(
            o.to_result()["text"]
                .as_str()
                .unwrap()
                .contains("exit status 3")
        );
    }

    #[test]
    fn the_environment_is_clean_and_the_folder_is_the_home() {
        // SAFETY: a test-only variable, set before the child is made.
        unsafe { std::env::set_var("LUCINA_DEVD_SECRET_TEST", "leak") };
        let o = sh("env | sort; pwd", 5);
        assert!(!o.text.contains("leak"), "{}", o.text);
        assert!(o.text.contains("PATH=/bin:/usr/bin"));
        assert!(o.text.contains("LANG=C.UTF-8"));
        assert!(
            o.text
                .contains(&format!("HOME={}", std::env::temp_dir().display()))
        );
        assert!(
            o.text
                .trim_end()
                .ends_with(&std::env::temp_dir().display().to_string())
        );
    }

    #[test]
    fn a_command_that_runs_too_long_is_stopped_with_what_it_started() {
        let t0 = Instant::now();
        let o = sh("sleep 30 & echo started; sleep 30", 1);
        assert!(o.timed_out);
        assert!(t0.elapsed() < Duration::from_secs(5));
        assert!(o.text.contains("started"));
        assert!(
            o.to_result()["text"]
                .as_str()
                .unwrap()
                .contains("timed out")
        );
    }

    #[test]
    fn output_is_cut_at_64_kib_and_the_command_still_finishes() {
        let o = sh("yes 0123456789 | head -c 200000; echo; echo done >&2", 10);
        assert!(o.truncated);
        assert!(o.text.len() <= MAX_OUTPUT + 64);
        assert_eq!(o.code, Some(0));
    }

    #[test]
    fn stdin_is_empty() {
        let o = sh("cat; echo end", 5);
        assert_eq!(o.text, "end\n");
    }
}
