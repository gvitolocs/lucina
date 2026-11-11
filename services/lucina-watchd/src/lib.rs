//! The parts of lucina-watchd and lucina-crash that do not touch the machine: the settings, when to restart a service, when
//! a link has stopped and when the whole network counts as dead, and what `/sys/fs/pstore/status` says.

use std::{net::Ipv4Addr, time::Duration};

/// What a service is: a name for the log and a command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Service {
    pub name: String,
    pub command: Vec<String>,
}

/// The settings, from the kernel command line as `lucina.watchd.<name>=<value>`, so that `lucina-reload --add` changes them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Settings {
    /// How long `/dev/watchdog` may go without a write; 0 leaves it closed.
    pub watchdog_timeout_s: u32,
    /// How long the network may stay dead after it was up; 0 never reboots for that.
    pub net_dead_s: u64,
    pub net_probe_every_s: u64,
    /// Whom to ask whether the network is alive, instead of the default gateway on port 80. Any answer counts, a refusal too.
    pub net_peer: Option<String>,
    pub services: Vec<Service>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            watchdog_timeout_s: 60,
            net_dead_s: 600,
            net_probe_every_s: 30,
            net_peer: None,
            services: vec![Service {
                name: "lucina-sshd".into(),
                command: vec!["/sbin/lucina-sshd".into()],
            }],
        }
    }
}

impl Settings {
    /// Reads `/proc/cmdline`'s text, up to `--`. Anything it does not understand stays at its default.
    pub fn from_cmdline(cmdline: &str) -> Self {
        let mut settings = Self::default();
        let args = cmdline
            .split_ascii_whitespace()
            .take_while(|arg| *arg != "--")
            .filter_map(|arg| arg.strip_prefix("lucina.watchd."));
        for arg in args {
            let Some((name, value)) = arg.split_once('=') else {
                continue;
            };
            match name {
                "timeout" => {
                    if let Ok(seconds) = value.parse() {
                        settings.watchdog_timeout_s = seconds;
                    }
                }
                "net_dead" => {
                    if let Ok(seconds) = value.parse() {
                        settings.net_dead_s = seconds;
                    }
                }
                "net_probe" => {
                    if let Ok(seconds) = value.parse::<u64>() {
                        settings.net_probe_every_s = seconds.max(1);
                    }
                }
                "net_peer" => settings.net_peer = Some(value.to_string()),
                _ => {}
            }
        }
        settings
    }

    /// How often to write to `/dev/watchdog`: a third of the limit, and no more than every second.
    pub fn pet_every(&self) -> Duration {
        Duration::from_secs(u64::from((self.watchdog_timeout_s / 3).max(1)))
    }
}

/// When to start a service again: 1 s after it died, doubling to a minute, and from 1 s again after a minute of running.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Backoff {
    failures: u32,
}

impl Backoff {
    pub const LONGEST_S: u64 = 60;

    /// The service died after running this long. Returns how long to wait.
    pub fn after_exit(&mut self, ran_for: Duration) -> Duration {
        if ran_for >= Duration::from_secs(Self::LONGEST_S) {
            self.failures = 0;
        }
        let delay = 1u64
            .checked_shl(self.failures)
            .unwrap_or(u64::MAX)
            .min(Self::LONGEST_S);
        self.failures = self.failures.saturating_add(1);
        Duration::from_secs(delay)
    }
}

/// What the network probes have found.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NetWatch {
    was_up: bool,
    /// The time of the first failed probe since the last good one.
    dead_since_s: Option<u64>,
}

/// What to do about the network.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NetVerdict {
    /// All is well, or there is no network to miss.
    Fine,
    /// A probe failed for the first time after working: worth a log line.
    JustDied,
    /// Still dead, but not for long enough.
    StillDead { for_s: u64 },
    /// Dead for longer than allowed.
    Reboot { for_s: u64 },
}

/// Whether a probe counts the network as alive: a link that carries, a peer that answers, or the shell's standby, which takes
/// Wi-Fi down on purpose (`/run/lucina/standby`, docs/research/juno-power.md "Idle standby"): no restart for that.
pub fn counts_alive(link_alive: bool, peer_answers: bool, standby: bool) -> bool {
    link_alive || peer_answers || standby
}

impl NetWatch {
    /// Takes a probe's result. A machine whose network never came up is left alone: no cable is no reason to reboot.
    pub fn probed(&mut self, alive: bool, now_s: u64, dead_limit_s: u64) -> NetVerdict {
        if alive {
            self.was_up = true;
            self.dead_since_s = None;
            return NetVerdict::Fine;
        }
        if !self.was_up {
            return NetVerdict::Fine;
        }
        let Some(since) = self.dead_since_s else {
            self.dead_since_s = Some(now_s);
            return NetVerdict::JustDied;
        };
        let for_s = now_s.saturating_sub(since);
        if dead_limit_s != 0 && for_s >= dead_limit_s {
            NetVerdict::Reboot { for_s }
        } else {
            NetVerdict::StillDead { for_s }
        }
    }
}

/// The default routes from `/proc/net/route`, as (interface, gateway). Addresses are hexadecimal there, in the machine's byte
/// order.
pub fn default_routes(route: &str) -> Vec<(String, Ipv4Addr)> {
    route
        .lines()
        .skip(1)
        .filter_map(|line| {
            let mut fields = line.split_ascii_whitespace();
            let (iface, destination, gateway) = (fields.next()?, fields.next()?, fields.next()?);
            if u32::from_str_radix(destination, 16).ok()? != 0 {
                return None;
            }
            let gateway = u32::from_str_radix(gateway, 16).ok()?;
            (gateway != 0).then(|| (iface.to_string(), Ipv4Addr::from(gateway.to_le_bytes())))
        })
        .collect()
}

/// The first default gateway in `/proc/net/route`.
pub fn default_gateway(route: &str) -> Option<Ipv4Addr> {
    default_routes(route)
        .into_iter()
        .next()
        .map(|(_, gateway)| gateway)
}

/// What each network link has done, round by round. A link is alive in a round when it received frames since the last
/// round or its gateway answered, as Linux's connectivity checks judge a device by its own traffic and its own route.
#[derive(Clone, Debug, Default)]
pub struct Links {
    links: Vec<Link>,
}

#[derive(Clone, Debug)]
struct Link {
    name: String,
    frames: u64,
    carried: bool,
    quiet_since_s: Option<u64>,
    told: bool,
}

/// What changed for a link.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LinkNews {
    /// A link that carried traffic has carried none for this long while another still does: worth a log line and a notice,
    /// not a restart.
    Stopped { name: String, for_s: u64 },
    /// A link told of as stopped carries traffic again.
    Back { name: String },
}

impl Links {
    /// Takes one round: each interface's received frames (`/proc/net/dev`) and the interfaces whose gateway answered.
    /// Returns whether any link is alive, and what changed. A link seen for the first time has no count to compare with,
    /// so only its gateway can make it alive in that round.
    pub fn round(
        &mut self,
        now_s: u64,
        frames: &[(String, u64)],
        answered: &[String],
        quiet_limit_s: u64,
    ) -> (bool, Vec<LinkNews>) {
        let mut seen: Vec<(String, bool)> = Vec::new();
        for (name, count) in frames {
            let grew = match self.links.iter_mut().find(|link| &link.name == name) {
                Some(link) => {
                    let grew = *count > link.frames;
                    link.frames = *count;
                    grew
                }
                None => {
                    self.links.push(Link::new(name, *count));
                    false
                }
            };
            seen.push((name.clone(), grew || answered.contains(name)));
        }
        for name in answered {
            if !seen.iter().any(|(seen_name, _)| seen_name == name) {
                if !self.links.iter().any(|link| &link.name == name) {
                    self.links.push(Link::new(name, 0));
                }
                seen.push((name.clone(), true));
            }
        }
        let any_alive = seen.iter().any(|(_, alive)| *alive);
        let mut news = Vec::new();
        for (name, alive) in seen {
            let Some(link) = self.links.iter_mut().find(|link| link.name == name) else {
                continue;
            };
            if alive {
                link.carried = true;
                link.quiet_since_s = None;
                if link.told {
                    link.told = false;
                    news.push(LinkNews::Back { name });
                }
            } else if link.carried {
                let since = *link.quiet_since_s.get_or_insert(now_s);
                let for_s = now_s.saturating_sub(since);
                if !link.told && quiet_limit_s != 0 && for_s >= quiet_limit_s && any_alive {
                    link.told = true;
                    news.push(LinkNews::Stopped { name, for_s });
                }
            }
        }
        (any_alive, news)
    }

    /// The links told of as stopped, which have not come back.
    pub fn stopped(&self) -> Vec<&str> {
        self.links
            .iter()
            .filter(|link| link.told)
            .map(|link| link.name.as_str())
            .collect()
    }
}

impl Link {
    fn new(name: &str, frames: u64) -> Self {
        Self {
            name: name.to_string(),
            frames,
            carried: false,
            quiet_since_s: None,
            told: false,
        }
    }
}

/// The frames each interface has received, from `/proc/net/dev` in Linux's format, without the loopback.
pub fn received_frames(dev: &str) -> Vec<(String, u64)> {
    dev.lines()
        .filter_map(|line| {
            let (name, counts) = line.split_once(':')?;
            let name = name.trim();
            if name.is_empty() || name.contains('|') || name == "lo" {
                return None;
            }
            let packets = counts.split_ascii_whitespace().nth(1)?.parse().ok()?;
            Some((name.to_string(), packets))
        })
        .collect()
}

/// The line for the person about links that stopped while another carries on.
pub fn link_notice(stopped: &[&str]) -> Option<String> {
    if stopped.is_empty() {
        return None;
    }
    let names: Vec<String> = stopped
        .iter()
        .map(|name| {
            if name.starts_with("wlan") {
                "Wi-Fi".to_string()
            } else {
                (*name).to_string()
            }
        })
        .collect();
    let verb = if names.len() == 1 { "carries" } else { "carry" };
    Some(format!(
        "Network: {} {verb} nothing any more. Another link still works, so nothing restarts.",
        names.join(" and ")
    ))
}

/// One saved crash in `/sys/fs/pstore/status`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Saved {
    pub slot: usize,
    pub seq: u32,
    pub boot: u32,
    pub kind: String,
    pub len: usize,
    pub reason: String,
}

/// What `/sys/fs/pstore/status` says.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Status {
    pub boot: u32,
    pub crashes_in_a_row: u32,
    pub last_boot: String,
    /// The format the saved log had before this boot moved it to the current one, if it did.
    pub format_upgraded_from: Option<u32>,
    pub saved: Vec<Saved>,
}

impl Status {
    /// Reads the file. Lines it does not know are skipped, so that a newer kernel can say more.
    pub fn parse(text: &str) -> Self {
        let mut status = Self::default();
        for line in text.lines() {
            if let Some(saved) = parse_record(line) {
                status.saved.push(saved);
                continue;
            }
            let mut words = line.split_ascii_whitespace();
            match (words.next(), words.next()) {
                (Some("boot"), Some(value)) => status.boot = value.parse().unwrap_or(0),
                (Some("crashes_in_a_row"), Some(value)) => {
                    status.crashes_in_a_row = value.parse().unwrap_or(0);
                }
                (Some("last_boot"), Some(value)) => status.last_boot = value.to_string(),
                (Some("format_upgraded_from"), Some(value)) => {
                    status.format_upgraded_from = value.parse().ok();
                }
                _ => {}
            }
        }
        status.saved.sort_by_key(|saved| saved.seq);
        status
    }

    /// The report of the last boot, if it ended badly: what a person logging in should hear about.
    pub fn crash_of_last_boot(&self) -> Option<&Saved> {
        self.saved
            .iter()
            .rev()
            .find(|saved| saved.boot + 1 == self.boot)
            .filter(|saved| self.last_boot != "clean" || saved.kind == "requested reset")
    }
}

/// `record <slot> seq <n> boot <n> kind <words> len <n> reason <words>`.
fn parse_record(line: &str) -> Option<Saved> {
    let rest = line.strip_prefix("record ")?;
    let (slot, rest) = rest.split_once(" seq ")?;
    let (seq, rest) = rest.split_once(" boot ")?;
    let (boot, rest) = rest.split_once(" kind ")?;
    let (kind, rest) = rest.split_once(" len ")?;
    let (len, reason) = rest.split_once(" reason ")?;
    Some(Saved {
        slot: slot.parse().ok()?,
        seq: seq.parse().ok()?,
        boot: boot.parse().ok()?,
        kind: kind.to_string(),
        len: len.parse().ok()?,
        reason: reason.to_string(),
    })
}

/// The login notice for a crash.
pub fn notice(system: &str, saved: &Saved) -> String {
    let what = if saved.kind == "requested reset" {
        "restarted itself"
    } else {
        "recovered from a crash"
    };
    format!(
        "{system} {what} ({}, boot {}): {}\nlucina-crash shows the report.\n",
        saved.kind, saved.boot, saved.reason
    )
}

/// Removes what a log's terminal could act on: colour sequences, control characters. Newlines and tabs stay.
pub fn clean_text(text: &[u8]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut bytes = text.iter().copied();
    while let Some(b) = bytes.next() {
        match b {
            0x1b => {
                if bytes.next() == Some(b'[') {
                    for c in bytes.by_ref() {
                        if (0x40..=0x7e).contains(&c) {
                            break;
                        }
                    }
                }
            }
            b'\n' | b'\t' | 0x20..=0x7e => out.push(b as char),
            b'\r' => {}
            _ => out.push('?'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_standby_with_wifi_down_is_no_dead_network() {
        let mut n = NetWatch::default();
        assert_eq!(
            n.probed(counts_alive(true, false, false), 0, 600),
            NetVerdict::Fine
        );
        // Wi-Fi down by the standby, no cable: alive for the judge, so no restart however long it lasts.
        for now in [30, 630, 6000] {
            assert_eq!(
                n.probed(counts_alive(false, false, true), now, 600),
                NetVerdict::Fine
            );
        }
        // Out of standby and still dead: the dead time counts from then, not from the standby's start.
        assert_eq!(
            n.probed(counts_alive(false, false, false), 6030, 600),
            NetVerdict::JustDied
        );
        assert!(counts_alive(false, true, false));
        assert!(!counts_alive(false, false, false));
    }

    #[test]
    fn settings_from_the_command_line() {
        let s = Settings::from_cmdline("");
        assert_eq!(s.watchdog_timeout_s, 60);
        assert_eq!(s.net_dead_s, 600);
        assert_eq!(s.services[0].command, ["/sbin/lucina-sshd"]);

        let s = Settings::from_cmdline(
            "console=tty0 lucina.watchd.timeout=9 lucina.watchd.net_dead=0 lucina.watchd.net_probe=5 \
             lucina.watchd.net_peer=1.2.3.4:22 lucina.watchd.nonsense=1 lucina.watchd.timeout -- lucina.watchd.timeout=99",
        );
        assert_eq!(s.watchdog_timeout_s, 9);
        assert_eq!(s.net_dead_s, 0);
        assert_eq!(s.net_probe_every_s, 5);
        assert_eq!(s.net_peer.as_deref(), Some("1.2.3.4:22"));

        assert_eq!(
            Settings::from_cmdline("lucina.watchd.timeout=x").watchdog_timeout_s,
            60
        );
        assert_eq!(
            Settings::from_cmdline("lucina.watchd.net_probe=0").net_probe_every_s,
            1
        );
    }

    #[test]
    fn pets_come_three_times_per_limit() {
        let mut s = Settings::default();
        assert_eq!(s.pet_every(), Duration::from_secs(20));
        s.watchdog_timeout_s = 2;
        assert_eq!(s.pet_every(), Duration::from_secs(1));
    }

    #[test]
    fn restarts_back_off_and_start_over() {
        let mut b = Backoff::default();
        let short = Duration::from_secs(1);
        let delays: Vec<u64> = (0..8).map(|_| b.after_exit(short).as_secs()).collect();
        assert_eq!(delays, [1, 2, 4, 8, 16, 32, 60, 60]);
        assert_eq!(b.after_exit(Duration::from_secs(60)).as_secs(), 1);
        assert_eq!(b.after_exit(short).as_secs(), 2);
        // A very long run of failures does not overflow.
        for _ in 0..200 {
            assert!(b.after_exit(short).as_secs() <= 60);
        }
    }

    #[test]
    fn a_network_that_was_never_up_is_left_alone() {
        let mut n = NetWatch::default();
        for now in [0, 1000, 100_000] {
            assert_eq!(n.probed(false, now, 600), NetVerdict::Fine);
        }
    }

    #[test]
    fn a_dead_network_reboots_after_the_limit() {
        let mut n = NetWatch::default();
        assert_eq!(n.probed(true, 0, 600), NetVerdict::Fine);
        assert_eq!(n.probed(false, 30, 600), NetVerdict::JustDied);
        assert_eq!(
            n.probed(false, 60, 600),
            NetVerdict::StillDead { for_s: 30 }
        );
        assert_eq!(n.probed(false, 630, 600), NetVerdict::Reboot { for_s: 600 });
    }

    #[test]
    fn a_network_that_comes_back_starts_over() {
        let mut n = NetWatch::default();
        n.probed(true, 0, 600);
        n.probed(false, 30, 600);
        assert_eq!(n.probed(true, 60, 600), NetVerdict::Fine);
        assert_eq!(n.probed(false, 700, 600), NetVerdict::JustDied);
        assert_eq!(
            n.probed(false, 730, 600),
            NetVerdict::StillDead { for_s: 30 }
        );
    }

    #[test]
    fn a_limit_of_zero_never_reboots() {
        let mut n = NetWatch::default();
        n.probed(true, 0, 0);
        n.probed(false, 1, 0);
        assert_eq!(
            n.probed(false, 1_000_000, 0),
            NetVerdict::StillDead { for_s: 999_999 }
        );
    }

    #[test]
    fn gateway_from_proc_net_route() {
        let route = "Iface\tDestination\tGateway\tFlags\tRefCnt\tUse\tMetric\tMask\tMTU\tWindow\tIRTT\n\
                     eth0\t0002000A\t00000000\t0001\t0\t0\t0\t00FFFFFF\t0\t0\t0\n\
                     eth0\t00000000\t0202000A\t0003\t0\t0\t0\t00000000\t0\t0\t0\n";
        assert_eq!(default_gateway(route), Some(Ipv4Addr::new(10, 0, 2, 2)));
        assert_eq!(default_gateway("Iface\n"), None);
        assert_eq!(default_gateway(""), None);
        assert_eq!(default_gateway("Iface\nnonsense\neth0 xx yy\n"), None);
    }

    const STATUS: &str = "boot 3\ncrashes_in_a_row 1\nlast_boot crashed\nfuture line\n\
        record 1 seq 7 boot 2 kind panic len 900 reason boom at x.rs:1\n\
        record 0 seq 6 boot 1 kind unexplained reset len 800 reason nothing recorded\n\
        record 9 seq broken\n";

    #[test]
    fn status_is_read_oldest_first() {
        let s = Status::parse(STATUS);
        assert_eq!(
            (s.boot, s.crashes_in_a_row, s.last_boot.as_str()),
            (3, 1, "crashed")
        );
        assert_eq!(s.saved.len(), 2);
        assert_eq!(s.saved[0].kind, "unexplained reset");
        assert_eq!(s.saved[1].reason, "boom at x.rs:1");
        assert_eq!((s.saved[1].slot, s.saved[1].len), (1, 900));
    }

    #[test]
    fn the_format_upgrade_line_is_read() {
        assert_eq!(Status::parse(STATUS).format_upgraded_from, None);
        let s = Status::parse("boot 4\nlast_boot clean\nformat_upgraded_from 3\n");
        assert_eq!(s.format_upgraded_from, Some(3));
        let s = Status::parse("boot 4\nformat_upgraded_from x\n");
        assert_eq!(s.format_upgraded_from, None);
    }

    #[test]
    fn the_notice_is_about_the_last_boot_only() {
        let s = Status::parse(STATUS);
        assert_eq!(s.crash_of_last_boot().unwrap().seq, 7);
        // An old crash, and a good boot since.
        let s = Status::parse(
            "boot 9\nlast_boot clean\nrecord 1 seq 7 boot 2 kind panic len 900 reason boom\n",
        );
        assert_eq!(s.crash_of_last_boot(), None);
        // A crash, then a clean restart: the crash is the boot before the last.
        let s = Status::parse(
            "boot 3\nlast_boot clean\nrecord 1 seq 7 boot 1 kind panic len 900 reason boom\n",
        );
        assert_eq!(s.crash_of_last_boot(), None);
        // A restart on request ended clean and is worth hearing about.
        let s = Status::parse(
            "boot 3\nlast_boot clean\nrecord 1 seq 7 boot 2 kind requested reset len 9 reason dead network\n",
        );
        assert_eq!(s.crash_of_last_boot().unwrap().reason, "dead network");
        assert_eq!(Status::parse("").crash_of_last_boot(), None);
    }

    #[test]
    fn notices_read_well() {
        let s = Status::parse(STATUS);
        assert_eq!(
            notice("Juno", s.crash_of_last_boot().unwrap()),
            "Juno recovered from a crash (panic, boot 2): boom at x.rs:1\nlucina-crash shows the report.\n"
        );
    }

    #[test]
    fn text_from_a_log_is_cleaned() {
        assert_eq!(
            clean_text(b"\x1b[31mred\x1b[0m\ta\x07b\xc3\r\n"),
            "red\ta?b?\n"
        );
    }

    fn frames(list: &[(&str, u64)]) -> Vec<(String, u64)> {
        list.iter().map(|(n, c)| (n.to_string(), *c)).collect()
    }

    #[test]
    fn one_quiet_link_is_news_not_a_dead_network() {
        let mut l = Links::default();
        let none: Vec<String> = Vec::new();
        l.round(0, &frames(&[("eth0", 10), ("wlan0", 10)]), &none, 600);
        assert_eq!(
            l.round(30, &frames(&[("eth0", 20), ("wlan0", 20)]), &none, 600),
            (true, vec![])
        );
        // Wi-Fi stops; Ethernet goes on.
        let mut eth = 20;
        for now in (60..=600).step_by(30) {
            eth += 5;
            let (alive, news) = l.round(now, &frames(&[("eth0", eth), ("wlan0", 20)]), &none, 600);
            assert!(alive);
            assert!(news.is_empty(), "too early at {now}: {news:?}");
        }
        eth += 5;
        let (alive, news) = l.round(660, &frames(&[("eth0", eth), ("wlan0", 20)]), &none, 600);
        assert!(alive);
        assert_eq!(
            news,
            vec![LinkNews::Stopped {
                name: "wlan0".into(),
                for_s: 600
            }]
        );
        assert_eq!(l.stopped(), vec!["wlan0"]);
        // Said once.
        eth += 5;
        assert_eq!(
            l.round(690, &frames(&[("eth0", eth), ("wlan0", 20)]), &none, 600),
            (true, vec![])
        );
        // And back.
        eth += 5;
        assert_eq!(
            l.round(720, &frames(&[("eth0", eth), ("wlan0", 21)]), &none, 600),
            (
                true,
                vec![LinkNews::Back {
                    name: "wlan0".into()
                }]
            )
        );
        assert!(l.stopped().is_empty());
    }

    #[test]
    fn a_gateway_that_answers_keeps_a_quiet_link_alive() {
        let mut l = Links::default();
        l.round(0, &frames(&[("eth0", 5)]), &[], 600);
        let answered = vec!["eth0".to_string()];
        for now in [30, 600, 1200] {
            assert_eq!(
                l.round(now, &frames(&[("eth0", 5)]), &answered, 600),
                (true, vec![])
            );
        }
    }

    #[test]
    fn every_link_quiet_is_a_dead_network_and_no_link_news() {
        let mut l = Links::default();
        l.round(0, &frames(&[("eth0", 1), ("wlan0", 1)]), &[], 600);
        l.round(30, &frames(&[("eth0", 2), ("wlan0", 2)]), &[], 600);
        for now in [60, 700, 1400] {
            assert_eq!(
                l.round(now, &frames(&[("eth0", 2), ("wlan0", 2)]), &[], 600),
                (false, vec![])
            );
        }
    }

    #[test]
    fn a_new_link_needs_a_second_round_or_its_gateway() {
        let mut l = Links::default();
        assert_eq!(
            l.round(0, &frames(&[("eth0", 100)]), &[], 600),
            (false, vec![])
        );
        assert_eq!(
            l.round(0, &frames(&[("eth1", 100)]), &["eth1".to_string()], 600),
            (true, vec![])
        );
    }

    #[test]
    fn a_link_that_never_carried_is_not_news() {
        let mut l = Links::default();
        l.round(0, &frames(&[("eth0", 1), ("wlan0", 0)]), &[], 60);
        for now in [30, 100, 1000] {
            let (_, news) = l.round(now, &frames(&[("eth0", now), ("wlan0", 0)]), &[], 60);
            assert!(news.is_empty());
        }
    }

    #[test]
    fn proc_net_dev_in_linuxs_format() {
        let dev = "Inter-|   Receive                                                |  Transmit\n \
                   face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed\n    \
                   lo: 0 7 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n  \
                   eth0: 0 30862 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n \
                   wlan0: 0 12 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n";
        assert_eq!(
            received_frames(dev),
            vec![("eth0".to_string(), 30862), ("wlan0".to_string(), 12)]
        );
        assert!(received_frames("").is_empty());
    }

    #[test]
    fn default_routes_of_every_link() {
        let route = "Iface\tDestination\tGateway\tFlags\tRefCnt\tUse\tMetric\tMask\tMTU\tWindow\tIRTT\n\
                     wlan0\t00000000\t0101A8C0\t0003\t0\t0\t0\t00000000\t0\t0\t0\n\
                     eth0\t00000000\t0202000A\t0003\t0\t0\t0\t00000000\t0\t0\t0\n";
        assert_eq!(
            default_routes(route),
            vec![
                ("wlan0".to_string(), Ipv4Addr::new(192, 168, 1, 1)),
                ("eth0".to_string(), Ipv4Addr::new(10, 0, 2, 2))
            ]
        );
    }

    #[test]
    fn the_notice_names_what_stopped() {
        assert_eq!(link_notice(&[]), None);
        assert_eq!(
            link_notice(&["wlan0"]).unwrap(),
            "Network: Wi-Fi carries nothing any more. Another link still works, so nothing restarts."
        );
        assert_eq!(
            link_notice(&["eth1", "wlan0"]).unwrap(),
            "Network: eth1 and Wi-Fi carry nothing any more. Another link still works, so nothing restarts."
        );
    }
}
