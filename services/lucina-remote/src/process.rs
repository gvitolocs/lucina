//! The program behind an SSH session channel: a command on pipes, or a shell on a pty.

use std::{
    io,
    os::{
        fd::{AsFd, BorrowedFd, OwnedFd},
        unix::{
            ffi::OsStrExt,
            process::{CommandExt, ExitStatusExt},
        },
    },
    process::{Child, Command, Stdio},
};

use rustix::{
    fs::{OFlags, fcntl_getfl, fcntl_setfl},
    io::Errno,
    pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt},
    termios::{Winsize, tcsetwinsize},
};

/// What the client asked for with `pty-req`.
#[derive(Clone, Debug)]
pub struct PtyRequest {
    pub term: String,
    pub cols: u16,
    pub rows: u16,
}

/// A running program and the parent's ends of its stdin, stdout and stderr.
///
/// With a pty, `stdin` and `stdout` are the same master and `stderr` is `None`.
pub struct Process {
    child: Child,
    pub stdin: Option<OwnedFd>,
    pub stdout: Option<OwnedFd>,
    pub stderr: Option<OwnedFd>,
    exit: Option<u32>,
}

/// Starts `/bin/sh -l` (no command) or `/bin/sh -c <command>` with the session's environment.
pub fn spawn(
    shell: &str,
    command: Option<&str>,
    pty: Option<&PtyRequest>,
    env: &[(String, String)],
) -> io::Result<Process> {
    let mut cmd = Command::new(shell);
    match command {
        Some(c) => cmd.arg("-c").arg(c),
        None => cmd.arg("-l"),
    };
    // root's home if it can be entered (it cannot when testing as another user on a host).
    let home = if rustix::fs::access("/root", rustix::fs::Access::EXEC_OK).is_ok() {
        "/root"
    } else {
        "/"
    };
    cmd.env_clear()
        .env("PATH", "/sbin:/usr/sbin:/bin:/usr/bin")
        .env("HOME", home)
        .env("USER", "root")
        .env("LOGNAME", "root")
        .env("SHELL", shell);
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.current_dir(home);

    match pty {
        None => {
            cmd.stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            // A session of its own, so that the program does not get the daemon's signals.
            // SAFETY: setsid is async-signal-safe and touches no memory of the parent.
            unsafe {
                cmd.pre_exec(|| rustix::process::setsid().map(drop).map_err(io::Error::from))
            };
            let mut child = cmd.spawn()?;
            let stdin = child.stdin.take().map(OwnedFd::from);
            let stdout = child.stdout.take().map(OwnedFd::from);
            let stderr = child.stderr.take().map(OwnedFd::from);
            for fd in [&stdin, &stdout, &stderr].into_iter().flatten() {
                set_nonblocking(fd.as_fd())?;
            }
            Ok(Process {
                child,
                stdin,
                stdout,
                stderr,
                exit: None,
            })
        }
        Some(p) => {
            let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC)?;
            grantpt(&master)?;
            unlockpt(&master)?;
            let name = ptsname(&master, Vec::new())?;
            let slave: OwnedFd = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(std::ffi::OsStr::from_bytes(name.as_bytes()))?
                .into();
            let size = Winsize {
                ws_row: p.rows.max(1),
                ws_col: p.cols.max(1),
                ws_xpixel: 0,
                ws_ypixel: 0,
            };
            // Some kernels have no TIOCSWINSZ on a pty yet; the shell then sees 0x0, which it survives.
            if let Err(e) = tcsetwinsize(&slave, size) {
                log::debug!("TIOCSWINSZ: {e}");
            }
            cmd.env("TERM", &p.term)
                .stdin(Stdio::from(slave.try_clone()?))
                .stdout(Stdio::from(slave.try_clone()?))
                .stderr(Stdio::from(slave));
            // SAFETY: setsid and TIOCSCTTY are async-signal-safe and touch no memory of the parent.
            unsafe {
                cmd.pre_exec(|| {
                    rustix::process::setsid()?;
                    rustix::process::ioctl_tiocsctty(rustix::stdio::stdin())?;
                    Ok(())
                })
            };
            let child = cmd.spawn()?;
            set_nonblocking(master.as_fd())?;
            let stdin = Some(master.try_clone()?);
            Ok(Process {
                child,
                stdin,
                stdout: Some(master),
                stderr: None,
                exit: None,
            })
        }
    }
}

fn set_nonblocking(fd: BorrowedFd<'_>) -> io::Result<()> {
    let flags = fcntl_getfl(fd)?;
    fcntl_setfl(fd, flags | OFlags::NONBLOCK)?;
    Ok(())
}

/// The result of a non-blocking read.
pub enum ReadResult {
    Data(usize),
    WouldBlock,
    Eof,
}

/// Reads without blocking. A pty master reports the end of the shell as EIO, which counts as EOF.
pub fn read_nb(fd: &OwnedFd, buf: &mut [u8]) -> ReadResult {
    match rustix::io::read(fd, buf) {
        Ok(0) => ReadResult::Eof,
        Ok(n) => ReadResult::Data(n),
        Err(Errno::AGAIN | Errno::INTR) => ReadResult::WouldBlock,
        Err(_) => ReadResult::Eof,
    }
}

/// Writes without blocking; returns how much was taken (0 if the pipe is full), or an error if the reader is gone.
pub fn write_nb(fd: &OwnedFd, buf: &[u8]) -> io::Result<usize> {
    match rustix::io::write(fd, buf) {
        Ok(n) => Ok(n),
        Err(Errno::AGAIN | Errno::INTR) => Ok(0),
        Err(e) => Err(e.into()),
    }
}

impl Process {
    /// The exit status once the program has ended, as SSH reports it (128 + signal for a signal).
    pub fn try_exit(&mut self) -> Option<u32> {
        if self.exit.is_none()
            && let Ok(Some(status)) = self.child.try_wait()
        {
            self.exit = Some(match (status.code(), status.signal()) {
                (Some(c), _) => c as u32,
                (None, Some(s)) => 128 + s as u32,
                _ => 255,
            });
        }
        self.exit
    }

    /// Hangs up on the program when the client goes away.
    pub fn kill(&mut self) {
        if self.try_exit().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
