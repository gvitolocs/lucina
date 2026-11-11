//! The app's switches for the hosts (RustDesk's `--host --direct`, the browser's `--host --web`): each a separate process, so
//! that it outlives the app, and nothing that survives a restart. Being reachable is asked for each time; after a reboot this
//! laptop is not reachable again until someone turns it on here.

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Which host a switch runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Host {
    /// A RustDesk client dials this laptop.
    Direct,
    /// A browser opens it.
    Web,
}

impl Host {
    fn pid_file(self) -> &'static str {
        match self {
            Host::Direct => "/run/lucina/desk-host.pid",
            Host::Web => "/run/lucina/desk-web.pid",
        }
    }

    fn log_file(self) -> &'static str {
        match self {
            Host::Direct => "/tmp/desk-host.log",
            Host::Web => "/tmp/desk-web.log",
        }
    }

    fn flag(self) -> &'static str {
        match self {
            Host::Direct => "--direct",
            Host::Web => "--web",
        }
    }
}

/// The process named in the file, if it is alive and is this program running that host.
pub fn running_pid(pid_file: &Path, flag: &str) -> Option<i32> {
    let pid: i32 = std::fs::read_to_string(pid_file).ok()?.trim().parse().ok()?;
    let cmd = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let mut args = cmd.split(|b| *b == 0);
    (args.clone().any(|a| a == b"--host") && args.any(|a| a == flag.as_bytes())).then_some(pid)
}

pub fn is_on(host: Host) -> bool {
    running_pid(Path::new(host.pid_file()), host.flag()).is_some()
}

/// Starts it in a session of its own, so that the shell ending this app leaves it alone. Its log goes to a file: this app's stderr
/// is a pipe that closes with the app.
pub fn turn_on(host: Host) -> Result<(), String> {
    if is_on(host) {
        return Ok(());
    }
    let exe = program_path();
    let log_file = host.log_file();
    let log = std::fs::OpenOptions::new().create(true).append(true).open(log_file).map_err(|e| format!("{log_file}: {e}"))?;
    let log2 = log.try_clone().map_err(|e| e.to_string())?;
    let mut c = Command::new(exe);
    c.args(["--host", host.flag()]).stdin(Stdio::null()).stdout(log).stderr(log2);
    // SAFETY: setsid is async-signal-safe, and nothing else runs between fork and exec.
    unsafe {
        c.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = c.spawn().map_err(|e| format!("the host did not start: {e}"))?;
    let pid_file = host.pid_file();
    if let Some(d) = Path::new(pid_file).parent() {
        let _ = std::fs::create_dir_all(d);
    }
    std::fs::write(pid_file, child.id().to_string()).map_err(|e| format!("{pid_file}: {e}"))
}

pub fn turn_off(host: Host) {
    if let Some(pid) = running_pid(Path::new(host.pid_file()), host.flag()) {
        // SAFETY: a signal to a process we just checked is ours.
        unsafe { libc::kill(pid, libc::SIGTERM) };
    }
    let _ = std::fs::remove_file(host.pid_file());
}

/// Where to start the host from. The shell runs an app from a copy it has already unlinked, which `/proc/self/exe` can still
/// start on a kernel that allows it; the copy in `/usr/bin` is the other way, on an image that has one.
fn program_path() -> PathBuf {
    let installed = Path::new("/usr/bin/lucina-app-desk");
    if installed.exists() { installed.to_path_buf() } else { PathBuf::from("/proc/self/exe") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stale_or_foreign_pid_is_not_the_host() {
        let d = std::env::temp_dir().join(format!("desk-control-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("pid");
        assert_eq!(running_pid(&f, "--web"), None);
        std::fs::write(&f, "not a number").unwrap();
        assert_eq!(running_pid(&f, "--web"), None);
        // This test program is alive and has no --host argument.
        std::fs::write(&f, std::process::id().to_string()).unwrap();
        assert_eq!(running_pid(&f, "--direct"), None);
        std::fs::write(&f, "999999999").unwrap();
        assert_eq!(running_pid(&f, "--web"), None);
        assert_ne!(Host::Web.pid_file(), Host::Direct.pid_file());
    }
}
