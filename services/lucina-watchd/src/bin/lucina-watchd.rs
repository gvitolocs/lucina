//! lucina-watchd: keeps Juno's services running and the machine honest. /etc/lucina/remote.sh starts it.
//!
//! - It starts the services (lucina-sshd) and restarts one that dies, waiting longer each time it dies again soon.
//! - It writes to `/dev/watchdog` from its main loop, so that a stuck userspace ends in a crash and a restart.
//! - It watches every network link: one is alive while it receives frames (`/proc/net/dev`) or its gateway answers on port 80,
//!   a refusal too. A link that stops while another carries on gets a log line and a notice for the person, and Wi-Fi one try
//!   to connect again; the machine does not restart for it. If the network was up and every link stays dead, it writes the
//!   kernel's USB state to the log and reboots, saying why.
//!
//! Settings: `lucina.watchd.timeout`, `.net_dead`, `.net_probe`, `.net_peer` on the kernel command line. See `Settings`.
// Juno with no C library: Eyra is the libc and the program start, in Rust (docs/research/juno-pure-rust.md).
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"))]
extern crate eyra;

use std::{
    ffi::CString,
    fs::{self, File, OpenOptions},
    io::Write,
    net::{SocketAddr, TcpStream, ToSocketAddrs},
    os::fd::AsRawFd,
    process::{Child, Command},
    sync::mpsc::{self, Receiver, TryRecvError},
    thread::{self, sleep},
    time::{Duration, Instant},
};

use lucina_watchd::{
    Backoff, LinkNews, Links, NetVerdict, NetWatch, Service, Settings, default_routes, link_notice,
    received_frames,
};

const TICK: Duration = Duration::from_secs(1);
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_PORT: u16 = 80;
const STALL: Duration = Duration::from_secs(10);
/// What the shell shows the person; lucina-trial writes there too.
const NOTICE: &str = "/run/lucina/notice";
/// Present while the shell's standby has Wi-Fi down on purpose: no Wi-Fi retry and no restart for a dead network meanwhile.
const STANDBY: &str = "/run/lucina/standby";
const NOTICE_PREFIX: &str = "Network: ";

fn main() {
    let cmdline = fs::read_to_string("/proc/cmdline").unwrap_or_default();
    let settings = Settings::from_cmdline(&cmdline);
    let mut log = Log::open();
    log.say(&format!(
        "started: watchdog limit {} s, network limit {} s",
        settings.watchdog_timeout_s, settings.net_dead_s
    ));

    let mut watchdog = Pet::open(&settings, &mut log);
    let mut services: Vec<Running> = settings
        .services
        .iter()
        .cloned()
        .map(Running::new)
        .collect();
    let started = Instant::now();
    let mut net = NetWatch::default();
    let mut links = Links::default();
    let mut wifi_retry: Option<Child> = None;
    let mut probe = None;
    let mut next_probe = Duration::ZERO;
    let mut last_round = Instant::now();

    loop {
        for service in &mut services {
            service.check(&mut log);
        }
        if let Some(watchdog) = &mut watchdog {
            watchdog.tick(&mut log);
        }

        let now = started.elapsed();
        if settings.net_dead_s != 0 && now >= next_probe && probe.is_none() {
            probe = Some(start_probe(&settings));
            next_probe = now + Duration::from_secs(settings.net_probe_every_s);
        }
        if let Some(answers) = probe_result(&mut probe) {
            let frames = fs::read_to_string("/proc/net/dev")
                .map(|dev| received_frames(&dev))
                .unwrap_or_default();
            let (link_alive, news) =
                links.round(now.as_secs(), &frames, &answers.links, settings.net_dead_s);
            let standby = std::path::Path::new(STANDBY).exists();
            for item in &news {
                match item {
                    LinkNews::Stopped { name, for_s } => {
                        log.say(&format!(
                            "{name} has carried nothing for {for_s} s while another link still does: no restart for it"
                        ));
                        if name.starts_with("wlan") && wifi_retry.is_none() && !standby {
                            wifi_retry = try_wifi_again(&mut log);
                        }
                    }
                    LinkNews::Back { name } => log.say(&format!("{name} carries traffic again")),
                }
            }
            if !news.is_empty() {
                set_notice(link_notice(&links.stopped()).as_deref());
            }
            match net.probed(
                lucina_watchd::counts_alive(link_alive, answers.peer, standby),
                now.as_secs(),
                settings.net_dead_s,
            ) {
                NetVerdict::Fine | NetVerdict::StillDead { .. } => {}
                NetVerdict::JustDied => log.say("no link carries anything and nothing answers"),
                NetVerdict::Reboot { for_s } => reboot_for_dead_network(for_s, &mut log),
            }
        }
        if let Some(child) = &mut wifi_retry
            && let Ok(Some(status)) = child.try_wait()
        {
            log.say(&format!("lucina-wifi connect --saved ended ({status})"));
            wifi_retry = None;
        }
        sleep(TICK);
        // A loop that stands still is what ends in the kernel's crash for a broken promise: say so while it can be said.
        if last_round.elapsed() >= STALL {
            log.say(&format!(
                "the main loop stood still for {} s",
                last_round.elapsed().as_secs()
            ));
        }
        last_round = Instant::now();
    }
}

/// The kernel log, where the lines end up in the saved log too.
struct Log(Option<File>);

impl Log {
    fn open() -> Self {
        Self(OpenOptions::new().write(true).open("/dev/kmsg").ok())
    }

    fn say(&mut self, line: &str) {
        // In one write: each write to /dev/kmsg is one record.
        let line = format!("lucina-watchd: {line}\n");
        let written = self
            .0
            .as_mut()
            .is_some_and(|kmsg| kmsg.write_all(line.as_bytes()).is_ok());
        if !written {
            print!("{line}");
        }
    }
}

/// A service and what is known of it.
struct Running {
    service: Service,
    child: Option<Child>,
    started: Instant,
    backoff: Backoff,
    start_at: Instant,
}

impl Running {
    fn new(service: Service) -> Self {
        let now = Instant::now();
        Self {
            service,
            child: None,
            started: now,
            backoff: Backoff::default(),
            start_at: now,
        }
    }

    fn check(&mut self, log: &mut Log) {
        let name = &self.service.name;
        if let Some(child) = &mut self.child {
            match child.try_wait() {
                Ok(None) => {}
                Ok(Some(status)) => {
                    let delay = self.backoff.after_exit(self.started.elapsed());
                    log.say(&format!(
                        "{name} ended ({status}) after {} s: starting it again in {} s",
                        self.started.elapsed().as_secs(),
                        delay.as_secs()
                    ));
                    self.child = None;
                    self.start_at = Instant::now() + delay;
                }
                Err(err) => log.say(&format!("{name}: cannot wait for it: {err}")),
            }
        }
        if self.child.is_none() && Instant::now() >= self.start_at {
            let (program, args) = self.service.command.split_first().expect("a command");
            match Command::new(program).args(args).spawn() {
                Ok(child) => {
                    log.say(&format!("{name} started, pid {}", child.id()));
                    self.child = Some(child);
                    self.started = Instant::now();
                }
                Err(err) => {
                    let delay = self.backoff.after_exit(Duration::ZERO);
                    log.say(&format!(
                        "{name}: cannot start it: {err}: again in {} s",
                        delay.as_secs()
                    ));
                    self.start_at = Instant::now() + delay;
                }
            }
        }
    }
}

/// `/dev/watchdog`, written from the main loop.
struct Pet {
    file: File,
    every: Duration,
    last: Instant,
}

impl Pet {
    fn open(settings: &Settings, log: &mut Log) -> Option<Self> {
        if settings.watchdog_timeout_s == 0 {
            log.say("leaving /dev/watchdog alone (lucina.watchd.timeout=0)");
            return None;
        }
        let file = match OpenOptions::new().write(true).open("/dev/watchdog") {
            Ok(file) => file,
            Err(err) => {
                log.say(&format!("no /dev/watchdog: {err}"));
                return None;
            }
        };
        // WDIOC_SETTIMEOUT: _IOWR('W', 6, int).
        const WDIOC_SETTIMEOUT: libc::c_ulong = 0xc004_5706;
        let mut timeout = settings.watchdog_timeout_s as libc::c_int;
        // SAFETY: the ioctl reads and writes one int, which lives through the call.
        if unsafe { libc::ioctl(file.as_raw_fd(), WDIOC_SETTIMEOUT as _, &mut timeout) } != 0 {
            log.say(&format!(
                "setting the limit of /dev/watchdog: {}",
                std::io::Error::last_os_error()
            ));
        }
        Some(Self {
            file,
            every: settings.pet_every(),
            last: Instant::now(),
        })
    }

    fn tick(&mut self, log: &mut Log) {
        if self.last.elapsed() >= self.every {
            // A failed write is a missed pet: the limit is three pets long.
            if let Err(err) = self.file.write_all(b".") {
                log.say(&format!("writing to /dev/watchdog: {err}"));
            }
            self.last = Instant::now();
        }
    }
}

/// What one round of probes found: the links whose gateway answered, and whether `lucina.watchd.net_peer` did.
struct Answers {
    links: Vec<String>,
    peer: bool,
}

/// The probes run in threads of their own: whatever the kernel's network does, the main loop must go on petting. Each link's
/// gateway is asked, or only `net_peer` when it is set.
fn start_probe(settings: &Settings) -> Receiver<Answers> {
    let peer = settings
        .net_peer
        .as_ref()
        .and_then(|peer| peer.to_socket_addrs().ok()?.next());
    let gateways = if peer.is_some() {
        Vec::new()
    } else {
        fs::read_to_string("/proc/net/route")
            .map(|route| default_routes(&route))
            .unwrap_or_default()
    };
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let asks: Vec<_> = gateways
            .into_iter()
            .map(|(name, gateway)| {
                let address = SocketAddr::from((gateway, DEFAULT_PORT));
                (name, thread::spawn(move || answers(address)))
            })
            .collect();
        let peer = peer.map(|address| thread::spawn(move || answers(address)));
        let links = asks
            .into_iter()
            .filter_map(|(name, ask)| ask.join().unwrap_or(false).then_some(name))
            .collect();
        let peer = peer.is_some_and(|ask| ask.join().unwrap_or(false));
        let _ = sender.send(Answers { links, peer });
    });
    receiver
}

/// Whether anybody answers at this address. A refusal is an answer: the path works.
fn answers(address: SocketAddr) -> bool {
    match TcpStream::connect_timeout(&address, PROBE_TIMEOUT) {
        Ok(_) => true,
        Err(err) => err.kind() == std::io::ErrorKind::ConnectionRefused,
    }
}

fn probe_result(probe: &mut Option<Receiver<Answers>>) -> Option<Answers> {
    match probe.as_ref()?.try_recv() {
        Ok(answers) => {
            *probe = None;
            Some(answers)
        }
        Err(TryRecvError::Empty) => None,
        Err(TryRecvError::Disconnected) => {
            *probe = None;
            None
        }
    }
}

/// Tries to bring Wi-Fi back alone, as far as its driver allows: a new association with the saved network. A driver that has
/// stopped, as when its modem died, cannot be started again without restarting the machine, which this does not do.
fn try_wifi_again(log: &mut Log) -> Option<Child> {
    let status = fs::read_to_string("/dev/lucina-wifi").unwrap_or_default();
    let state: Vec<&str> = status
        .lines()
        .filter(|line| line.starts_with("state ") || line.starts_with("failed "))
        .collect();
    log.say(&format!(
        "Wi-Fi: {}",
        if state.is_empty() {
            "no status".to_string()
        } else {
            state.join("; ")
        }
    ));
    if status.lines().any(|line| line == "state stopped") {
        log.say("Wi-Fi's driver has stopped and cannot be started again alone: Wi-Fi stays off until the next restart");
        return None;
    }
    match Command::new("/sbin/lucina-wifi")
        .args(["connect", "--saved"])
        .spawn()
    {
        Ok(child) => {
            log.say(&format!(
                "Wi-Fi: connecting again to the saved network, pid {}",
                child.id()
            ));
            Some(child)
        }
        Err(err) => {
            log.say(&format!("Wi-Fi: cannot run lucina-wifi: {err}"));
            None
        }
    }
}

/// Puts this line in the notice the shell shows, or takes it out. Other lines there stay.
fn set_notice(line: Option<&str>) {
    let old = fs::read_to_string(NOTICE).unwrap_or_default();
    let mut lines: Vec<&str> = old
        .lines()
        .filter(|old_line| !old_line.starts_with(NOTICE_PREFIX) && !old_line.trim().is_empty())
        .collect();
    lines.extend(line);
    if lines.is_empty() {
        let _ = fs::remove_file(NOTICE);
        return;
    }
    let _ = fs::create_dir_all("/run/lucina");
    let _ = fs::write(NOTICE, lines.join("\n") + "\n");
}

/// Before a planned restart: `lucina-data clean --cap 5`, so that the next boot does not run e2fsck on /data (it has no journal).
/// Best effort; waits 6 s at most, and says what happened.
fn mark_data_clean() -> String {
    if !std::path::Path::new("/run/lucina/data-ready").exists() {
        return "no /data on this boot".to_string();
    }
    let mut child = match Command::new("/sbin/lucina-data")
        .args(["clean", "--cap", "5"])
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return format!("/data not marked clean (lucina-data: {e}), next boot checks");
        }
    };
    let started = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(s)) if s.success() => return "/data marked clean".to_string(),
            Ok(Some(s)) => {
                return format!("/data not marked clean (lucina-data {s}), next boot checks");
            }
            Ok(None) if started.elapsed() > Duration::from_secs(6) => {
                let _ = child.kill();
                return "/data not marked clean (lucina-data took too long); the next boot checks it".to_string();
            }
            Ok(None) => sleep(Duration::from_millis(50)),
            Err(e) => return format!("/data not marked clean ({e}), next boot checks"),
        }
    }
}

/// Writes the USB state to the log and asks for a restart, with the reason in the saved log. `LINUX_REBOOT_CMD_RESTART2` is
/// Linux's restart with a text.
fn reboot_for_dead_network(for_s: u64, log: &mut Log) -> ! {
    log.say(&format!(
        "the network has been dead for {for_s} s; the USB state:"
    ));
    let usb = fs::read_to_string("/proc/lucina-usb")
        .unwrap_or_else(|err| format!("(no USB state: {err})\n"));
    for line in usb.lines() {
        log.say(line);
    }
    let reason = format!("lucina-watchd: the network was up and has been dead for {for_s} s");
    log.say(&reason);
    log.say(&mark_data_clean());
    let text = CString::new(reason).expect("no NUL in it");
    const MAGIC1: libc::c_long = 0xfee1_dead;
    const MAGIC2: libc::c_long = 0x2812_1969;
    const RESTART2: libc::c_long = 0xa1b2_c3d4;
    // SAFETY: the kernel reads a NUL-terminated string that lives through the call.
    unsafe { libc::syscall(libc::SYS_reboot, MAGIC1, MAGIC2, RESTART2, text.as_ptr()) };
    let failed = std::io::Error::last_os_error();
    log.say(&format!("the restart failed: {failed}"));
    // /data was marked clean and is out of use: back to a normal read-write mount that is not clean (best effort).
    let _ = Command::new("/sbin/lucina-data").arg("reopen").status();
    // Stop petting, and the kernel ends it.
    loop {
        sleep(Duration::from_secs(60));
    }
}
