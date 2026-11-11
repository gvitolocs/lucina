//! Running apps: each is a program of its own (`crates/lucina-app-*`) that gets the framebuffer and
//! the input devices while it is on screen. The shell stops drawing, but keeps reading input for one
//! key: F1 sends the app's process group SIGTERM, and SIGKILL half a second later. Whatever the app
//! does, it is another process in another session: it cannot take the shell with it.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use lucina_app::Manifest;
use lucina_app::manifest;
use lucina_remote::{appset, esp};

const LUN0: &str = "/dev/ufs0lun0";

/// How long an app gets to save and leave after SIGTERM.
pub const GRACE: Duration = Duration::from_millis(500);
/// How long a SIGKILLed process may take to go before the shell draws anyway.
const AFTER_KILL: Duration = Duration::from_secs(2);
/// How many lines of the app's stderr the crash screen can show.
const TAIL_LINES: usize = 8;

/// How long the disk may take to appear after the shell has drawn.
const DISK_WAIT: Duration = Duration::from_secs(20);
/// Where a program from the disk runs from: RAM, and only while it runs.
const RUN_DIR: &str = "/tmp/lucina-apps";

/// The big apps (the editors) live on the persistent partition, not in the image: they cost a
/// generation to change there, and do not fit in it.
pub const DATA_APPS: &str = "/data/lucina/apps";
/// The file the data daemon writes when /data can be used (`lucina-data`).
pub const DATA_READY: &str = "/run/lucina/data-ready";

/// Waits for the disk, mounts the EFI partition read-only and reads the manifests of the set in use. On its own thread:
/// nothing on screen waits for it.
pub fn read_disk_apps() -> Result<(String, Vec<Manifest>), String> {
    let t0 = Instant::now();
    while !Path::new(LUN0).exists() {
        if t0.elapsed() > DISK_WAIT {
            return Err(format!(
                "no disk after {} s (is the kernel running with ufs=on?)",
                DISK_WAIT.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    ensure_mounted()?;
    let (dir, generation) = appset::current_folder(Path::new(appset::APPS_MOUNT))?;
    let mut apps = manifest::scan(&dir, &mut |e| eprintln!("lucina-shell: {e}"));
    for m in &mut apps {
        m.exec = dir
            .join(format!("{}.bin", m.id))
            .to_string_lossy()
            .into_owned();
    }
    Ok((generation, apps))
}

fn ensure_mounted() -> Result<(), String> {
    let device = esp::partition()?;
    if esp::mounted_at(&device).as_deref() == Some(appset::APPS_MOUNT) {
        return Ok(());
    }
    esp::mount_read_only(&device, appset::APPS_MOUNT)
}

/// The apps on the persistent partition: the same layout as a set's folder, `<id>.app`,
/// `<id>.bin`, optionally `<id>.png`. The folder does not exist until the first app is
/// installed, and that is no error. A manifest without a program that can run is reported
/// and left out.
pub fn read_data_apps(dir: &Path) -> Vec<Manifest> {
    let apps = manifest::scan(dir, &mut |e| eprintln!("lucina-shell: {e}"));
    let mut kept = Vec::with_capacity(apps.len());
    for m in apps {
        let bin = dir.join(format!("{}.bin", m.id));
        let meta = match std::fs::symlink_metadata(&bin) {
            Ok(meta) => meta,
            Err(_) => {
                eprintln!("lucina-shell: {}: no program in {bin:?}", m.id);
                continue;
            }
        };
        // A symlink is not a program: /data could unmount out from under it.
        if !meta.file_type().is_file() {
            eprintln!(
                "lucina-shell: {}: the program is not a regular file: {bin:?}",
                m.id
            );
            continue;
        }
        if meta.permissions().mode() & 0o111 == 0 {
            eprintln!(
                "lucina-shell: {}: the program is not executable: {bin:?}",
                m.id
            );
            continue;
        }
        let mut m = m;
        m.exec = bin.to_string_lossy().into_owned();
        kept.push(m);
    }
    kept
}

/// The apps found on /data join the set: an id that is already there from the image or the disk
/// wins, and the one from /data is dropped with its reason. A rescan finds the data's own apps
/// again, and they stay where they are. What comes back has the new apps after the old ones.
pub fn merge_data_apps(mut set: Vec<Manifest>, found: Vec<Manifest>) -> Vec<Manifest> {
    for m in found {
        if let Some(a) = set.iter().find(|a| a.id == m.id) {
            // The one already there is the data's own, from the last scan.
            if !a.exec.starts_with(DATA_APPS) {
                eprintln!(
                    "lucina-shell: /data: {} is already in the set: the one on /data is dropped",
                    m.id
                );
            }
            continue;
        }
        set.push(m);
    }
    set
}

/// The program to start: a file in the image as it is, a file from the disk copied to RAM, a file
/// on /data as it is. So nothing runs from the EFI partition, which can always be unmounted, and
/// a running app costs only its own pages. Returns the path, and whether it is a copy to remove
/// once the program has started.
pub fn prepare(m: &Manifest) -> io::Result<(String, bool)> {
    if m.exec.starts_with(DATA_APPS) {
        // /data stays mounted while the shell runs: the app and its files stay in one place.
        return Ok((m.exec.clone(), false));
    }
    if !m.exec.starts_with(appset::APPS_MOUNT) {
        return Ok((m.exec.clone(), false));
    }
    // A tool that writes to the partition unmounted it.
    ensure_mounted().map_err(io::Error::other)?;
    std::fs::create_dir_all(RUN_DIR)?;
    let copy = format!("{RUN_DIR}/{}", m.id);
    let _ = std::fs::remove_file(&copy);
    std::fs::copy(&m.exec, &copy)?;
    std::fs::set_permissions(&copy, std::fs::Permissions::from_mode(0o755))?;
    Ok((copy, true))
}

/// An app's folder with its files: one on /data runs from the apps' folder there, a disk app from
/// its set's folder on the partition, and the rest from the folder of the manifests in the image.
/// It becomes `LUCINA_APP_DIR`.
pub fn app_dir(m: &Manifest, copy: bool) -> String {
    if m.exec.starts_with(DATA_APPS) {
        return DATA_APPS.to_string();
    }
    let dir = Path::new(&m.exec).parent().filter(|_| copy);
    dir.map_or(manifest::MANIFEST_DIR.to_string(), |d| {
        d.to_string_lossy().into_owned()
    })
}

/// The line typed into the terminal for a program that runs there.
pub fn terminal_command(path: &str, dir: &str) -> String {
    let quote = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
    format!("LUCINA_APP_DIR={} {}\n", quote(dir), quote(path))
}

/// What happened to an app, for the screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ended {
    /// The app's position in the manifest list, to reopen it.
    pub app: usize,
    pub title: String,
    /// One sentence.
    pub what: String,
    /// The last lines it wrote to stderr.
    pub tail: Vec<String>,
    /// It failed, as opposed to being closed.
    pub failed: bool,
}

pub struct Running {
    pub app: usize,
    pub manifest: Manifest,
    child: Child,
    output: Option<File>,
    pidfd: Option<OwnedFd>,
    /// The read end of the app's hold descriptor (`lucina_app::hold`), until the app closes it.
    hold_pipe: Option<File>,
    hold_partial: Vec<u8>,
    /// The screen is held on by this app, for this reason.
    display_hold: Option<String>,
    tail: VecDeque<String>,
    partial: Vec<u8>,
    /// F1 was pressed at this time.
    stopping: Option<Instant>,
    killed: bool,
}

fn signal_name(n: i32) -> String {
    match n {
        libc::SIGSEGV => "a memory error (SIGSEGV)".into(),
        libc::SIGBUS => "a bus error (SIGBUS)".into(),
        libc::SIGABRT => "an abort, usually a panic (SIGABRT)".into(),
        libc::SIGILL => "an illegal instruction (SIGILL)".into(),
        libc::SIGFPE => "an arithmetic error (SIGFPE)".into(),
        libc::SIGKILL => "SIGKILL".into(),
        libc::SIGTERM => "SIGTERM".into(),
        n => format!("signal {n}"),
    }
}

/// Whether a line of stderr belongs on the crash screen: not the toolkit's trace, nor Rust's advice.
fn worth_showing(line: &str) -> bool {
    !(line.trim().is_empty()
        || line.contains(": frame at_us=")
        || line.contains(": key ")
        || line.starts_with("note: run with `RUST_BACKTRACE"))
}

/// What the exit status means. `asked` is true when the user went home, so ending was expected.
pub fn describe(status: ExitStatus, asked: bool, killed: bool) -> (String, bool) {
    if killed {
        return (
            "It did not close in time after F1, so it was stopped.".into(),
            false,
        );
    }
    match (status.code(), status.signal()) {
        (Some(0), _) => ("It closed.".into(), false),
        (Some(n), _) if !asked => (format!("It stopped with exit status {n}."), true),
        (Some(_), _) => ("It closed.".into(), false),
        (_, Some(libc::SIGTERM)) if asked => ("It closed.".into(), false),
        (_, Some(s)) => (format!("It stopped because of {}.", signal_name(s)), true),
        _ => ("It stopped.".into(), true),
    }
}

impl Running {
    /// The app's process id, for the tree's "front" (lucina-ax).
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Start an app. It gets the time zone rule, and tells the toolkit it has a shell.
    /// With `trace` the toolkit logs its frames too.
    /// `app_dir` is the folder of its files beside the program (`<id>.pkg`). `args` follow the program's path, as a file
    /// given to Video or Player (`lucina-devd`'s `play_file`); a tile gives none.
    pub fn start(
        app: usize,
        manifest: &Manifest,
        app_dir: &str,
        tz: &str,
        trace: bool,
        args: &[String],
        env: &[(String, String)],
    ) -> io::Result<Running> {
        let mut pipe = [-1; 2];
        // SAFETY: pipe2 fills the two-element array with new descriptors.
        if unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_NONBLOCK | libc::O_CLOEXEC) } < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: both descriptors were just made and belong to nobody else.
        let (read_end, write_end) =
            unsafe { (File::from_raw_fd(pipe[0]), OwnedFd::from_raw_fd(pipe[1])) };
        // The app's end must block, or its eprintln! fails when the pipe is full.
        // SAFETY: changes the flags of a descriptor we own. (O_NONBLOCK of the read end is kept: a
        // pipe's two ends have their own file status flags.)
        unsafe { libc::fcntl(write_end.as_raw_fd(), libc::F_SETFL, 0) };
        // The hold descriptor (`lucina_app::hold`): the app's end becomes descriptor 3 in the child; the shell's end does not block.
        let mut hold = [-1; 2];
        // SAFETY: pipe2 fills the two-element array with new descriptors.
        if unsafe { libc::pipe2(hold.as_mut_ptr(), libc::O_NONBLOCK | libc::O_CLOEXEC) } < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: both descriptors were just made and belong to nobody else.
        let (hold_read, mut hold_write) =
            unsafe { (File::from_raw_fd(hold[0]), OwnedFd::from_raw_fd(hold[1])) };
        // SAFETY: the app's end blocks, as its output pipe does.
        unsafe { libc::fcntl(hold_write.as_raw_fd(), libc::F_SETFL, 0) };
        // std's spawn keeps a close-on-exec pipe in the child to report exec errors; if that
        // pipe's write end lands on descriptor 3, the child-side dup2 would replace it. Guard
        // without relying on std internals: if descriptor 3 is free in the parent, move the app's
        // end there now, so std's pipe cannot get it. (If 3 is taken by something else in the
        // parent, it is not std's pipe, which std creates during spawn, and the child-side dup2
        // is fine as it is.)
        // SAFETY: F_GETFD on 3 only asks whether the descriptor is open.
        let fd3_free = unsafe { libc::fcntl(lucina_app::hold::FD, libc::F_GETFD) } < 0
            && io::Error::last_os_error().raw_os_error() == Some(libc::EBADF);
        let hold_child = if fd3_free && hold_write.as_raw_fd() != lucina_app::hold::FD {
            // SAFETY: F_DUPFD_CLOEXEC duplicates hold_write onto the lowest free descriptor >= 3,
            // which is exactly 3 because 3 is free; the new descriptor belongs to us alone.
            let moved = unsafe {
                libc::fcntl(
                    hold_write.as_raw_fd(),
                    libc::F_DUPFD_CLOEXEC,
                    lucina_app::hold::FD,
                )
            };
            if moved < 0 {
                return Err(io::Error::last_os_error());
            }
            // hold_write still owns the old descriptor: swap it for the one at 3, so each is
            // closed exactly once in the parent.
            let old = std::mem::replace(
                &mut hold_write,
                // SAFETY: moved is a new descriptor that nothing else owns.
                unsafe { OwnedFd::from_raw_fd(moved) },
            );
            drop(old);
            moved
        } else {
            hold_write.as_raw_fd()
        };
        let mut cmd = Command::new(&manifest.exec);
        cmd.args(args)
            .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .env("LUCINA_SHELL", "1")
            .env("LUCINA_APP_ID", &manifest.id)
            .env("LUCINA_APP_DIR", app_dir)
            .env(lucina_app::hold::ENV, lucina_app::hold::FD.to_string())
            .env("TZ", tz)
            .stdin(Stdio::null())
            .stdout(Stdio::from(write_end.try_clone()?))
            .stderr(Stdio::from(write_end));
        if lucina_app::icons::is_name(&manifest.icon) {
            cmd.env("LUCINA_APP_ICON", &manifest.icon);
        }
        if trace {
            cmd.env("LUCINA_APP_TRACE", "1");
        }
        // SAFETY: setsid is async-signal-safe. A session of its own: SIGTERM and SIGKILL reach the
        // app and whatever it started, and nothing else.
        unsafe {
            cmd.pre_exec(move || {
                if libc::setsid() < 0 {
                    return Err(io::Error::last_os_error());
                }
                if hold_child == lucina_app::hold::FD {
                    // dup2(3, 3) is a no-op and does NOT clear close-on-exec, so the app would
                    // lose its hold descriptor at exec: clear the flag by hand instead.
                    if libc::fcntl(lucina_app::hold::FD, libc::F_SETFD, 0) < 0 {
                        return Err(io::Error::last_os_error());
                    }
                } else {
                    // dup2 clears close-on-exec on the copy: descriptor 3 is the one the app keeps.
                    if libc::dup2(hold_child, lucina_app::hold::FD) < 0 {
                        return Err(io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
        let child = cmd.spawn()?;
        drop(hold_write);
        // SAFETY: pidfd_open takes a pid and flags. Without it the shell looks every 200 ms.
        let pidfd = unsafe { libc::syscall(libc::SYS_pidfd_open, child.id() as libc::c_int, 0) };
        let pidfd = (pidfd >= 0).then(|| {
            // SAFETY: a new descriptor that nothing else owns.
            unsafe { OwnedFd::from_raw_fd(pidfd as RawFd) }
        });
        Ok(Running {
            app,
            manifest: manifest.clone(),
            child,
            output: Some(read_end),
            pidfd,
            hold_pipe: Some(hold_read),
            hold_partial: Vec::new(),
            display_hold: None,
            tail: VecDeque::new(),
            partial: Vec::new(),
            stopping: None,
            killed: false,
        })
    }

    /// What to wake up for, besides input.
    pub fn fds(&self) -> Vec<RawFd> {
        self.pidfd
            .iter()
            .map(|f| f.as_raw_fd())
            .chain(self.output.iter().map(|f| f.as_raw_fd()))
            .chain(self.hold_pipe.iter().map(|f| f.as_raw_fd()))
            .collect()
    }

    /// When to look again if nothing wakes the shell.
    pub fn next_check(&self, now: Instant) -> Instant {
        let every = if self.pidfd.is_some() {
            Duration::from_secs(2)
        } else {
            Duration::from_millis(200)
        };
        match self.stopping {
            Some(t) if !self.killed => (t + GRACE).max(now + Duration::from_millis(1)),
            Some(_) => now + Duration::from_millis(50),
            None => now + every,
        }
    }

    fn signal_group(&self, sig: i32) {
        let pid = self.child.id() as libc::pid_t;
        // SAFETY: plain kills. The app leads its own session, so -pid is its group. Early on it
        // may not have called setsid yet, so the process itself is signalled too.
        unsafe {
            libc::kill(-pid, sig);
            libc::kill(pid, sig);
        }
    }

    /// The screen went off or came back: an app has nobody to draw for while it is off.
    pub fn set_paused(&self, paused: bool) {
        self.signal_group(if paused { libc::SIGSTOP } else { libc::SIGCONT });
    }

    /// F1: ask the app to end.
    pub fn stop(&mut self, now: Instant) {
        if self.stopping.is_none() {
            self.stopping = Some(now);
            self.signal_group(libc::SIGTERM);
        }
    }

    /// Who holds the screen on: this app, with its reason, while it says so and its descriptor is open.
    pub fn display_hold(&self) -> Option<&str> {
        self.display_hold.as_deref()
    }

    /// Read the app's hold lines. End of file (the app ended, crashed or was killed) drops the hold.
    fn pump_hold(&mut self) {
        let Some(f) = self.hold_pipe.as_mut() else {
            return;
        };
        let mut buf = [0u8; 512];
        let mut eof = false;
        loop {
            match f.read(&mut buf) {
                Ok(0) => {
                    eof = true;
                    break;
                }
                Ok(n) => self.hold_partial.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(_) => {
                    eof = true;
                    break;
                }
            }
        }
        while let Some(i) = self.hold_partial.iter().position(|&b| b == b'\n') {
            let line = String::from_utf8_lossy(&self.hold_partial[..i]).into_owned();
            self.hold_partial.drain(..=i);
            match lucina_app::hold::parse_line(&line) {
                Some(lucina_app::hold::Message::Hold(reason)) => self.display_hold = Some(reason),
                Some(lucina_app::hold::Message::Release) => self.display_hold = None,
                None => {}
            }
        }
        if self.hold_partial.len() > 4096 {
            self.hold_partial.clear();
        }
        if eof {
            self.hold_pipe = None;
            self.display_hold = None;
        }
    }

    /// Copy what the app wrote to our stderr, and keep the last lines.
    fn pump_output(&mut self) {
        let Some(f) = self.output.as_mut() else {
            return;
        };
        let mut buf = [0u8; 4096];
        let mut eof = false;
        loop {
            match f.read(&mut buf) {
                Ok(0) => {
                    eof = true;
                    break;
                }
                Ok(n) => self.partial.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(_) => {
                    eof = true;
                    break;
                }
            }
        }
        if eof {
            self.output = None;
            if self.partial.last().is_some_and(|&b| b != b'\n') {
                self.partial.push(b'\n');
            }
        }
        while let Some(i) = self.partial.iter().position(|&b| b == b'\n') {
            let line = String::from_utf8_lossy(&self.partial[..i]).into_owned();
            self.partial.drain(..=i);
            eprintln!("app {}| {line}", self.manifest.id);
            if !worth_showing(&line) {
                continue;
            }
            if self.tail.len() == TAIL_LINES {
                self.tail.pop_front();
            }
            self.tail.push_back(line);
        }
        // A line without an end must not grow for ever.
        if self.partial.len() > 64 * 1024 {
            self.partial.clear();
        }
    }

    /// Look at the app: the report when it has ended.
    pub fn check(&mut self, now: Instant) -> Option<Ended> {
        self.pump_output();
        self.pump_hold();
        let status = match self.child.try_wait() {
            Ok(Some(s)) => Some(s),
            Ok(None) => None,
            Err(e) => {
                eprintln!("lucina-shell: waiting for {}: {e}", self.manifest.id);
                None
            }
        };
        let (what, failed) = match status {
            Some(s) => {
                // Whatever it started goes with it.
                self.display_hold = None;
                self.signal_group(libc::SIGKILL);
                self.pump_output();
                describe(s, self.stopping.is_some(), self.killed)
            }
            None => {
                let t = self.stopping?;
                if !self.killed && now.duration_since(t) >= GRACE {
                    self.killed = true;
                    self.signal_group(libc::SIGKILL);
                }
                if now.duration_since(t) < GRACE + AFTER_KILL {
                    return None;
                }
                // It will not die: draw anyway, so that home always comes back.
                (
                    "It does not respond to being stopped, and may still be running.".to_string(),
                    true,
                )
            }
        };
        Some(Ended {
            app: self.app,
            title: self.manifest.title.clone(),
            what,
            tail: self.tail.iter().cloned().collect(),
            failed,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_terminal_program_is_typed_with_its_folder() {
        assert_eq!(
            terminal_command("/tmp/lucina-apps/coder", "/mnt/esp/g-1"),
            "LUCINA_APP_DIR='/mnt/esp/g-1' '/tmp/lucina-apps/coder'\n"
        );
        assert_eq!(
            terminal_command("/a'b", "/d"),
            "LUCINA_APP_DIR='/d' '/a'\\''b'\n"
        );
    }

    /// A script file that is open for writing while another test thread forks is held open by the forked child until its exec, and
    /// exec of a file with a writer fails with ETXTBSY ("Text file busy"): seen on the CI runner as a flaky start() error in
    /// a_killed_app_leaves_no_hold. Writing the scripts and forking the apps never overlap: both take this lock.
    static SPAWN: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn spawn_lock() -> std::sync::MutexGuard<'static, ()> {
        SPAWN.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn start_serialized(
        app: usize,
        manifest: &Manifest,
        app_dir: &str,
        tz: &str,
        trace: bool,
        args: &[String],
        env: &[(String, String)],
    ) -> io::Result<Running> {
        let _g = spawn_lock();
        Running::start(app, manifest, app_dir, tz, trace, args, env)
    }

    fn manifest(script: &str) -> (Manifest, std::path::PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let _g = spawn_lock();
        let dir = std::env::temp_dir().join(format!(
            "lucina-shell-apps-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let exec = dir.join("app.sh");
        std::fs::write(&exec, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&exec, std::fs::Permissions::from_mode(0o755)).unwrap();
        (
            Manifest {
                id: "t".into(),
                title: "Test".into(),
                exec: exec.to_str().unwrap().into(),
                icon: "T".into(),
                needs_network: false,
                terminal: false,
                intents: Vec::new(),
            },
            dir,
        )
    }

    /// A manifest with an exec that need not exist on disk.
    fn manifest_at(id: &str, exec: &str) -> Manifest {
        Manifest {
            id: id.into(),
            title: format!("T-{id}"),
            exec: exec.into(),
            icon: "T".into(),
            needs_network: false,
            terminal: false,
            intents: Vec::new(),
        }
    }

    #[test]
    fn an_app_on_data_runs_from_the_partition() {
        // The folder need not exist: prepare must not touch the disk at all.
        let m = manifest_at("artcraft", &format!("{DATA_APPS}/artcraft.bin"));
        let (path, copy) = prepare(&m).unwrap();
        assert_eq!(path, m.exec);
        assert!(!copy);
        assert_eq!(app_dir(&m, copy), DATA_APPS);
    }

    #[test]
    fn an_image_app_still_uses_the_manifests_folder() {
        let m = manifest_at(
            "hello",
            &format!("{}/lucina-app-hello", manifest::MANIFEST_DIR),
        );
        let (path, copy) = prepare(&m).unwrap();
        assert_eq!(path, m.exec);
        assert!(!copy);
        assert_eq!(app_dir(&m, copy), manifest::MANIFEST_DIR);
    }

    #[test]
    fn read_data_apps_keeps_only_what_can_run() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let dir = std::env::temp_dir().join(format!(
            "lucina-shell-data-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        // The good app: a manifest, a program, and a picture.
        std::fs::write(dir.join("good.app"), "id = good\ntitle = Good\n").unwrap();
        let bin = dir.join("good.bin");
        std::fs::write(&bin, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(dir.join("good.png"), b"\x89PNG").unwrap();
        // No program at all.
        std::fs::write(dir.join("nobin.app"), "id = nobin\ntitle = No bin\n").unwrap();
        // A program that is a link.
        std::fs::write(dir.join("link.app"), "id = link\ntitle = Link\n").unwrap();
        symlink(&bin, dir.join("link.bin")).unwrap();
        // A program that cannot run.
        std::fs::write(dir.join("noexec.app"), "id = noexec\ntitle = No exec\n").unwrap();
        let noexec = dir.join("noexec.bin");
        std::fs::write(&noexec, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&noexec, std::fs::Permissions::from_mode(0o644)).unwrap();

        let apps = read_data_apps(&dir);
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].id, "good");
        assert_eq!(apps[0].exec, bin.to_string_lossy().into_owned());
        assert_eq!(
            apps[0].icon,
            dir.join("good.png").to_string_lossy().into_owned()
        );
    }

    #[test]
    fn read_data_apps_on_a_missing_folder_is_empty() {
        assert!(read_data_apps(Path::new("/nonexistent-data-apps")).is_empty());
    }

    #[test]
    fn a_data_app_whose_id_is_already_there_is_dropped() {
        let set = vec![manifest_at(
            "artcraft",
            &format!("{}/lucina-app-artcraft", manifest::MANIFEST_DIR),
        )];
        let found = vec![
            manifest_at("artcraft", &format!("{DATA_APPS}/artcraft.bin")),
            manifest_at("video", &format!("{DATA_APPS}/video.bin")),
        ];
        let merged = merge_data_apps(set, found);
        assert_eq!(
            merged.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["artcraft", "video"]
        );
        // What stays is the image's app, not the one on /data.
        assert!(merged[0].exec.starts_with(manifest::MANIFEST_DIR));
        assert_eq!(merged[1].exec, format!("{DATA_APPS}/video.bin"));
    }

    fn wait(r: &mut Running) -> Ended {
        let t0 = Instant::now();
        loop {
            if let Some(e) = r.check(Instant::now()) {
                return e;
            }
            assert!(t0.elapsed() < Duration::from_secs(20), "app did not end");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn the_crash_screen_leaves_out_trace_and_advice() {
        assert!(worth_showing("deliberate panic"));
        assert!(!worth_showing(""));
        assert!(!worth_showing("x: frame at_us=1 draw_us=2"));
        assert!(!worth_showing("x: key 30"));
        assert!(!worth_showing(
            "note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace"
        ));
    }

    #[test]
    fn an_intent_reaches_the_app_in_its_environment() {
        let (m, d) =
            manifest("echo \"call=$LUCINA_INTENT reply=$LUCINA_INTENT_REPLY\" >&2; exit 0");
        let env = [
            (
                "LUCINA_INTENT".to_string(),
                r#"{"name":"notes.open"}"#.to_string(),
            ),
            (
                "LUCINA_INTENT_REPLY".to_string(),
                "/run/lucina/intents/c1.json".to_string(),
            ),
        ];
        let mut r = start_serialized(0, &m, "/", "UTC0", false, &[], &env).unwrap();
        wait(&mut r);
        assert!(
            r.tail
                .iter()
                .any(|l| l == r#"call={"name":"notes.open"} reply=/run/lucina/intents/c1.json"#),
            "{:?}",
            r.tail
        );
        std::fs::remove_dir_all(d).ok();
    }

    #[test]
    fn args_follow_the_program() {
        let (m, d) = manifest("echo \"got $1|$2\" >&2; exit 0");
        let args = ["/data/home/a b.mp4".to_string(), "x".to_string()];
        let mut r = start_serialized(0, &m, "/", "UTC0", false, &args, &[]).unwrap();
        wait(&mut r);
        assert!(
            r.tail.iter().any(|l| l == "got /data/home/a b.mp4|x"),
            "{:?}",
            r.tail
        );
        std::fs::remove_dir_all(d).ok();
    }

    #[test]
    fn a_clean_exit_is_not_a_failure() {
        let (m, d) = manifest("exit 0");
        let mut r = start_serialized(3, &m, "/", "UTC0", false, &[], &[]).unwrap();
        let e = wait(&mut r);
        assert_eq!(e.app, 3);
        assert_eq!(e.what, "It closed.");
        assert!(!e.failed);
        std::fs::remove_dir_all(d).ok();
    }

    #[test]
    fn a_failure_says_why_with_the_last_lines() {
        let (m, d) =
            manifest("echo first >&2\necho \"id $LUCINA_APP_ID shell $LUCINA_SHELL\"\nexit 3");
        let mut r = start_serialized(0, &m, "/", "UTC0", false, &[], &[]).unwrap();
        let e = wait(&mut r);
        assert!(e.failed);
        assert_eq!(e.what, "It stopped with exit status 3.");
        assert_eq!(e.tail, ["first", "id t shell 1"]);
        std::fs::remove_dir_all(d).ok();
    }

    #[test]
    fn a_signal_is_a_failure() {
        let (m, d) = manifest("kill -SEGV $$");
        let mut r = start_serialized(0, &m, "/", "UTC0", false, &[], &[]).unwrap();
        let e = wait(&mut r);
        assert!(e.failed);
        assert!(e.what.contains("SIGSEGV"), "{}", e.what);
        std::fs::remove_dir_all(d).ok();
    }

    #[test]
    fn stop_ends_a_polite_app_and_kills_a_stubborn_one() {
        // sh ends on SIGTERM by default: that is the user going home, not a failure.
        let (m, d) = manifest("sleep 100");
        let mut r = start_serialized(0, &m, "/", "UTC0", false, &[], &[]).unwrap();
        r.stop(Instant::now());
        let e = wait(&mut r);
        assert!(!e.failed, "{}", e.what);
        std::fs::remove_dir_all(d).ok();

        let (m, d) = manifest("trap '' TERM\necho ready\nwhile :; do sleep 1; done");
        let mut r = start_serialized(0, &m, "/", "UTC0", false, &[], &[]).unwrap();
        // Until it has set its trap.
        let t0 = Instant::now();
        while r.tail.is_empty() {
            r.pump_output();
            assert!(t0.elapsed() < Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(5));
        }
        let t0 = Instant::now();
        r.stop(t0);
        let e = wait(&mut r);
        assert!(t0.elapsed() >= GRACE);
        assert!(!e.failed);
        assert!(e.what.contains("did not close in time"), "{}", e.what);
        std::fs::remove_dir_all(d).ok();
    }

    #[test]
    fn a_program_that_cannot_start_is_an_error() {
        let m = Manifest {
            id: "x".into(),
            title: "X".into(),
            exec: "/nonexistent/program".into(),
            icon: "X".into(),
            needs_network: false,
            terminal: false,
            intents: Vec::new(),
        };
        assert!(start_serialized(0, &m, "/", "UTC0", false, &[], &[]).is_err());
    }

    /// Look at the app until `want` holds, for at most ten seconds.
    fn until(r: &mut Running, want: impl Fn(&Running) -> bool) {
        let t0 = Instant::now();
        while !want(r) {
            r.pump_hold();
            assert!(
                t0.elapsed() < Duration::from_secs(10),
                "hold now {:?}",
                r.display_hold()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn an_app_holds_the_screen_and_lets_it_go() {
        let (m, d) = manifest(
            "echo 'display-required on playing video' >&3\nsleep 0.3\necho 'display-required off' >&3\nsleep 100",
        );
        let mut r = start_serialized(0, &m, "/", "UTC0", false, &[], &[]).unwrap();
        until(&mut r, |r| r.display_hold() == Some("playing video"));
        until(&mut r, |r| r.display_hold().is_none());
        r.stop(Instant::now());
        wait(&mut r);
        std::fs::remove_dir_all(d).ok();
    }

    #[test]
    fn closing_the_descriptor_ends_the_hold() {
        let (m, d) = manifest("echo 'display-required on x' >&3\nexec 3>&-\nsleep 100");
        let mut r = start_serialized(0, &m, "/", "UTC0", false, &[], &[]).unwrap();
        // Either the line or the end of file is seen first; the end of file must leave no hold while the app still runs.
        until(&mut r, |r| r.hold_pipe.is_none());
        assert_eq!(r.display_hold(), None);
        assert!(r.child.try_wait().unwrap().is_none(), "still running");
        r.stop(Instant::now());
        wait(&mut r);
        std::fs::remove_dir_all(d).ok();
    }

    #[test]
    fn a_killed_app_leaves_no_hold() {
        // A child keeps descriptor 3 open after the app dies: the exit alone must drop the hold.
        let (m, d) = manifest("echo 'display-required on playing' >&3\nsleep 100 &\nwait");
        let mut r = start_serialized(0, &m, "/", "UTC0", false, &[], &[]).unwrap();
        until(&mut r, |r| r.display_hold() == Some("playing"));
        // SAFETY: a plain kill of the app's own process, not its group.
        unsafe { libc::kill(r.pid() as libc::pid_t, libc::SIGKILL) };
        let e = wait(&mut r);
        assert!(e.failed, "{}", e.what);
        assert_eq!(r.display_hold(), None);
        std::fs::remove_dir_all(d).ok();
    }
}
