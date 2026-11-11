//! The terminal's shell: a pseudo-terminal pair from `/dev/ptmx` with BusyBox `sh -l` on the
//! slave side as a session leader, so job control and Ctrl+C work as on a console.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};

const TIOCSWINSZ: libc::c_ulong = 0x5414;
const TIOCSCTTY: libc::c_ulong = 0x540e;
const TIOCGPTN: libc::c_ulong = 0x8004_5430;
const TIOCSPTLCK: libc::c_ulong = 0x4004_5431;

fn os_error(what: &str) -> io::Error {
    let e = io::Error::last_os_error();
    io::Error::new(e.kind(), format!("{what}: {e}"))
}

pub struct Pty {
    master: File,
    child: Child,
}

impl Pty {
    /// Start `argv` on a new pty of `cols` x `rows`, in the time zone `tz` (a POSIX rule).
    pub fn spawn(argv: &[&str], cols: u16, rows: u16, tz: &str) -> io::Result<Pty> {
        let master = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOCTTY | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open("/dev/ptmx")?;
        let fd = master.as_raw_fd();
        let unlock: libc::c_int = 0;
        // SAFETY: TIOCSPTLCK reads an int; TIOCGPTN writes one.
        if unsafe { libc::ioctl(fd, TIOCSPTLCK as _, &unlock) } < 0 {
            return Err(os_error("TIOCSPTLCK"));
        }
        let mut n: libc::c_uint = 0;
        if unsafe { libc::ioctl(fd, TIOCGPTN as _, &mut n) } < 0 {
            return Err(os_error("TIOCGPTN"));
        }
        let ws = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: TIOCSWINSZ reads a struct winsize.
        if unsafe { libc::ioctl(fd, TIOCSWINSZ as _, &ws) } < 0 {
            return Err(os_error("TIOCSWINSZ"));
        }
        let slave = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOCTTY)
            .open(format!("/dev/pts/{n}"))?;
        let mut cmd = Command::new(argv[0]);
        cmd.args(&argv[1..])
            .stdin(Stdio::from(slave.try_clone()?))
            .stdout(Stdio::from(slave.try_clone()?))
            .stderr(Stdio::from(slave))
            .env("TERM", "linux")
            .env("TZ", tz)
            .env("COLUMNS", cols.to_string())
            .env("LINES", rows.to_string())
            .current_dir("/");
        // SAFETY: only async-signal-safe calls between fork and exec: a new session, then the
        // pty (already on fd 0) becomes its controlling terminal.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::ioctl(0, TIOCSCTTY as _, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = crate::spawn::spawn(&mut cmd)?;
        Ok(Pty { master, child })
    }

    pub fn fd(&self) -> i32 {
        self.master.as_raw_fd()
    }

    /// Output available now; `Ok(None)` once the shell has gone (EOF or EIO on the master).
    pub fn read(&mut self, buf: &mut Vec<u8>) -> io::Result<Option<usize>> {
        let mut chunk = [0u8; 4096];
        let mut total = 0;
        loop {
            match self.master.read(&mut chunk) {
                Ok(0) => return Ok(if total > 0 { Some(total) } else { None }),
                Ok(n) => {
                    buf.extend_from_slice(&chunk[..n]);
                    total += n;
                    if total >= 64 * 1024 {
                        return Ok(Some(total));
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(Some(total)),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                // Linux returns EIO on the master once the last slave fd is closed.
                Err(e) if e.raw_os_error() == Some(libc::EIO) => {
                    return Ok(if total > 0 { Some(total) } else { None });
                }
                Err(e) => return Err(e),
            }
        }
    }

    pub fn write(&mut self, bytes: &[u8]) {
        let mut rest = bytes;
        while !rest.is_empty() {
            match self.master.write(rest) {
                Ok(0) => return,
                Ok(n) => rest = &rest[n..],
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                // The shell is not reading (a full pty buffer): drop the rest rather than block
                // the whole interface.
                Err(_) => return,
            }
        }
    }

    /// Whether the shell has exited (reaps it).
    pub fn exited(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)) | Err(_))
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        if !self.exited() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
