//! What the running system reports, turned into events as it happens: lucina-eventd's kernel probes (a program killed by a
//! fault signal, a link going down, a low battery) and the kernel log's errors that the system carried on from (Windows'
//! LiveKernelEvent). Repeats are folded: one event per signature an hour, carrying how many it stands for, so a driver that
//! logs the same error every second costs the log one line an hour, not thousands (Event Viewer's main noise).

use std::collections::HashMap;

use serde_json::json;

use crate::bucket;
use crate::event::{Channel, Event, Level, LogLine};
use crate::providers::{self, id};
use crate::record;
use crate::rules;

/// At most one event an hour per signature.
pub const THROTTLE_MS: i64 = 3_600_000;

/// Signals that mean a program faulted, not that someone stopped it (SIGKILL, SIGTERM and SIGINT are left out).
pub fn fault_signal(sig: i64) -> Option<&'static str> {
    Some(match sig {
        4 => "an illegal instruction (SIGILL)",
        5 => "a breakpoint trap (SIGTRAP)",
        6 => "an abort (SIGABRT, often a Rust panic)",
        7 => "a bus error (SIGBUS)",
        8 => "an arithmetic fault (SIGFPE)",
        11 => "a segmentation fault (SIGSEGV)",
        31 => "a forbidden system call (SIGSYS)",
        _ => return None,
    })
}

/// Words in a kernel log line that make it a non-fatal error worth a line in the log.
const ERROR_WORDS: &[&str] = &[
    "failed",
    "error",
    "timed out",
    "timeout",
    "crashed",
    "stalled",
    "not respond",
    "did not",
    "lost ",
];
/// Lines that use those words without being an error.
const NOT_ERRORS: &[&str] = &[
    ": alive at ",
    "is ignored",
    "errors 0",
    "error 0",
    " 0 errors",
    "lost 0",
];

#[derive(Default)]
pub struct Follower {
    exe: HashMap<i64, String>,
    seen: HashMap<String, rules::Gate>,
    pub boot: Option<u32>,
}

impl Follower {
    pub fn new(boot: Option<u32>) -> Follower {
        Follower {
            boot,
            ..Default::default()
        }
    }

    /// Lets one event per signature through an hour; the next one through says how many were held back.
    fn throttle(&mut self, sig: &str, now_ms: i64) -> Option<u64> {
        // `rules::throttle` (Verus-checked) wants a clock from 1970 on and a count that can still grow.
        let now = now_ms.max(0);
        let prev = self.seen.get(sig).copied().map(|g| rules::Gate {
            last_ms: g.last_ms.max(0),
            held: g.held.min(u64::MAX - 1),
        });
        let p = rules::throttle(prev, now, THROTTLE_MS);
        self.seen.insert(sig.to_string(), p.gate);
        p.pass.then_some(p.held_back)
    }

    fn stamp(&self, mut e: Event, now_ms: i64) -> Event {
        e.time_ms = Some(now_ms);
        e.boot = self.boot;
        e.activity = self.boot.map(|b| format!("boot:{b}"));
        e
    }

    /// One event from lucina-eventd's queue (kind, fields as JSON).
    pub fn on_hub(
        &mut self,
        kind: &str,
        fields: &serde_json::Map<String, serde_json::Value>,
        t_ms: u64,
        now_ms: i64,
    ) -> Option<Event> {
        let int = |n: &str| fields.get(n).and_then(|v| v.as_i64());
        match kind {
            "proc.started" => {
                if let (Some(pid), Some(exe)) =
                    (int("pid"), fields.get("exe").and_then(|v| v.as_str()))
                {
                    if self.exe.len() > 4096 {
                        self.exe.clear();
                    }
                    self.exe.insert(pid, exe.to_string());
                }
                None
            }
            "proc.exited" => {
                let pid = int("pid")?;
                let exe = self.exe.remove(&pid);
                let sig = int("signal")?;
                let what = fault_signal(sig)?;
                let exe = exe.unwrap_or_else(|| format!("pid {pid}"));
                let prog = exe.rsplit('/').next().unwrap_or(&exe).to_string();
                let app = prog.strip_prefix("lucina-app-").map(str::to_string);
                let name = app
                    .clone()
                    .map(|a| capitalize(&a))
                    .unwrap_or_else(|| prog.clone());
                let b = bucket::make(
                    &[("program", prog.clone()), ("signal", sig.to_string())],
                    format!("{prog} · signal {sig}"),
                );
                let held = self.throttle(&b.id, now_ms)?;
                let channel = if app.is_some() {
                    Channel::Apps
                } else {
                    Channel::System
                };
                let mut e = Event::new(
                    providers::APPS,
                    id::APP_CRASHED,
                    Level::Error,
                    channel,
                    format!("{name} ended on {what}."),
                )
                .field("program", exe.as_str())
                .field("pid", pid)
                .field("signal", sig);
                if let Some(a) = app {
                    e = e.field("app", a);
                }
                if held > 0 {
                    e = e.field("held_back", held);
                }
                e.uptime_ms = Some(t_ms);
                e.bucket = Some(b);
                e.next_step = Some(format!(
                    "Run {prog} from a terminal to see its last output, or read its log under /run/lucina."
                ));
                Some(self.stamp(e, now_ms))
            }
            // The kernel's OOM killer (patch 1118) ended a process: an app crash, named, with the memory it held.
            "proc.oom_killed" => {
                let pid = int("pid")?;
                let exe = self.exe.remove(&pid).or_else(|| {
                    fields.get("exe").and_then(|v| v.as_str()).map(str::to_string)
                });
                let exe = exe.unwrap_or_else(|| format!("pid {pid}"));
                let prog = exe.rsplit('/').next().unwrap_or(&exe).to_string();
                let app = prog.strip_prefix("lucina-app-").map(str::to_string);
                let name = app
                    .clone()
                    .map(|a| capitalize(&a))
                    .unwrap_or_else(|| prog.clone());
                let rss_mb = int("rss_kb").unwrap_or(0) / 1024;
                let b = bucket::make(
                    &[("program", prog.clone()), ("reason", "out of memory".to_string())],
                    format!("{prog} · out of memory"),
                );
                let held = self.throttle(&b.id, now_ms)?;
                let channel = if app.is_some() {
                    Channel::Apps
                } else {
                    Channel::System
                };
                let mut e = Event::new(
                    providers::APPS,
                    id::APP_CRASHED,
                    Level::Error,
                    channel,
                    format!("{name} was ended because memory ran out ({rss_mb} MB in use)."),
                )
                .field("program", exe.as_str())
                .field("pid", pid)
                .field("signal", 9)
                .field("oom", true)
                .field("rss_kb", int("rss_kb").unwrap_or(0));
                if let Some(a) = app {
                    e = e.field("app", a);
                }
                if held > 0 {
                    e = e.field("held_back", held);
                }
                e.uptime_ms = Some(t_ms);
                e.bucket = Some(b);
                e.next_step = Some(format!(
                    "{prog} used the most memory when it ran out; the system stayed up. Open it again, with fewer tabs or a smaller window."
                ));
                Some(self.stamp(e, now_ms))
            }
            "net.down" => {
                let iface = fields
                    .get("iface")
                    .and_then(|v| v.as_str())
                    .unwrap_or("a link")
                    .to_string();
                let sig = format!("net.down:{iface}");
                let held = self.throttle(&sig, now_ms)?;
                let mut e = Event::new(
                    providers::NET,
                    id::LINK_STOPPED,
                    Level::Warning,
                    Channel::System,
                    format!("The network link {iface} went down."),
                )
                .field("iface", iface);
                if held > 0 {
                    e = e.field("held_back", held);
                }
                e.uptime_ms = Some(t_ms);
                Some(self.stamp(e, now_ms))
            }
            "power.charger" => {
                let online = fields.get("online").and_then(|v| v.as_bool())?;
                let mut e = Event::new(
                    providers::POWER,
                    id::POWER_SOURCE,
                    Level::Info,
                    Channel::System,
                    if online {
                        "The charger was plugged in.".to_string()
                    } else {
                        "The charger was unplugged: on battery.".to_string()
                    },
                )
                .field("online", online);
                e.uptime_ms = Some(t_ms);
                Some(self.stamp(e, now_ms))
            }
            "power.battery" => {
                let level = int("level")?;
                let charging = fields
                    .get("charging")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if level > 10 || charging {
                    return None;
                }
                self.throttle(&format!("battery:{}", level / 5), now_ms)?;
                let mut e = Event::new(
                    providers::POWER,
                    id::BATTERY_LOW,
                    Level::Warning,
                    Channel::System,
                    format!("The battery is at {level} % and not charging."),
                )
                .field("level", level);
                e.uptime_ms = Some(t_ms);
                e.next_step = Some("Plug in the charger.".into());
                Some(self.stamp(e, now_ms))
            }
            _ => None,
        }
    }

    /// One kernel log line (/dev/kmsg form). Errors the system carried on from become warnings.
    pub fn on_kmsg(&mut self, line: &str, now_ms: i64) -> Option<Event> {
        // `<prio>,<seq>,<usec>,<flags>;text`: priority 3 or less is an error by the kernel's own account.
        let prio = line
            .split(',')
            .next()
            .and_then(|p| p.parse::<u32>().ok())
            .map(|p| p & 7);
        let l = record::parse_log_line(line);
        let lower = l.text.to_lowercase();
        if NOT_ERRORS.iter().any(|w| lower.contains(w)) {
            return None;
        }
        let worded = ERROR_WORDS.iter().any(|w| lower.contains(w));
        if !(worded || prio.is_some_and(|p| p <= 3)) {
            return None;
        }
        let sub = record::subsystem(&l.text).map(str::to_string);
        if sub.is_none() && !lower.starts_with("failed to allocate") {
            return None;
        }
        let norm = bucket::normalize(&l.text);
        let b = bucket::make(
            &[("kind", "kernel warning".into()), ("line", norm.clone())],
            norm.clone(),
        );
        let held = self.throttle(&b.id, now_ms)?;
        let who = sub
            .as_deref()
            .map(crate::analyze::subsystem_words)
            .unwrap_or("the kernel");
        let mut e = Event::new(
            providers::KERNEL_LIVE,
            id::KERNEL_WARNING,
            Level::Warning,
            Channel::System,
            format!(
                "{} reported an error and carried on: \"{}\".",
                capitalize(who),
                l.text.chars().take(140).collect::<String>()
            ),
        );
        e.uptime_ms = l.uptime_ms;
        if held > 0 {
            e.fields.insert("held_back".into(), json!(held));
            e.summary
                .push_str(&format!(" ({held} more like it in the last hour.)"));
        }
        e.log = vec![LogLine {
            uptime_ms: l.uptime_ms,
            text: l.text,
            mark: Some("this line".into()),
        }];
        e.bucket = Some(b);
        Some(self.stamp(e, now_ms))
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Map;

    fn f(v: serde_json::Value) -> Map<String, serde_json::Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn an_oom_kill_is_an_app_crash() {
        let mut fo = Follower::new(Some(18));
        fo.on_hub(
            "proc.started",
            &f(json!({"pid": 50, "ppid": 1, "exe": "/usr/bin/lucina-app-video"})),
            10,
            0,
        );
        let e = fo
            .on_hub(
                "proc.oom_killed",
                &f(json!({"pid": 50, "exe": "lucina-app-video", "rss_kb": 3_000_000, "adj": 0, "free_kb": 8192})),
                20,
                1000,
            )
            .unwrap();
        assert_eq!(e.summary, "Video was ended because memory ran out (2929 MB in use).");
        assert_eq!(e.channel, Channel::Apps);
        assert_eq!(e.fields["oom"], json!(true));
        // Without its start seen, the kernel's name is used.
        let e = fo
            .on_hub("proc.oom_killed", &f(json!({"pid": 77, "exe": "lucinajs", "rss_kb": 1024})), 30, 2000)
            .unwrap();
        assert_eq!(e.summary, "lucinajs was ended because memory ran out (1 MB in use).");
        assert_eq!(e.channel, Channel::System);
        // Its exit with SIGKILL afterwards is not a second crash.
        assert!(fo.on_hub("proc.exited", &f(json!({"pid": 77, "signal": 9})), 30, 2100).is_none());
    }

    #[test]
    fn an_app_that_faults_is_named() {
        let mut fo = Follower::new(Some(17));
        assert!(
            fo.on_hub(
                "proc.started",
                &f(json!({"pid": 42, "ppid": 1, "exe": "/usr/bin/lucina-app-gallery"})),
                10,
                0
            )
            .is_none()
        );
        let e = fo
            .on_hub(
                "proc.exited",
                &f(json!({"pid": 42, "signal": 11})),
                20,
                1000,
            )
            .unwrap();
        assert_eq!(
            e.summary,
            "Gallery ended on a segmentation fault (SIGSEGV)."
        );
        assert_eq!(e.channel, Channel::Apps);
        assert_eq!(e.boot, Some(17));
        assert_eq!(e.fields["app"], json!("gallery"));
        // A kill is not a crash, an exit code neither.
        assert!(
            fo.on_hub("proc.exited", &f(json!({"pid": 43, "signal": 9})), 20, 1000)
                .is_none()
        );
        assert!(
            fo.on_hub("proc.exited", &f(json!({"pid": 44, "code": 1})), 20, 1000)
                .is_none()
        );
    }

    #[test]
    fn repeats_are_held_back_an_hour() {
        let mut fo = Follower::new(None);
        let line = "3,100,5000000,-;wifi: wmi: command 0x3001 timed out";
        assert!(fo.on_kmsg(line, 0).is_some());
        assert!(
            fo.on_kmsg("3,101,6000000,-;wifi: wmi: command 0x3002 timed out", 1000)
                .is_none()
        );
        let e = fo.on_kmsg(line, THROTTLE_MS + 1).unwrap();
        assert_eq!(e.fields["held_back"], json!(1));
        assert!(
            e.summary
                .starts_with("Wi-Fi reported an error and carried on")
        );
    }

    #[test]
    fn heartbeats_and_spam_are_not_errors() {
        let mut fo = Follower::new(None);
        assert!(
            fo.on_kmsg(
                "6,1,1,-;wifi: alive at 5 ms, Stats { bad_msg: 0, error 0 }",
                0
            )
            .is_none()
        );
        assert!(
            fo.on_kmsg("4,2,1,-;POSIX_FADV_WILLNEED is ignored", 0)
                .is_none()
        );
        assert!(fo.on_kmsg("6,3,1,-;venus: session 3: opened", 0).is_none());
        assert!(
            fo.on_kmsg("6,4,1,-;Failed to allocate a large slot", 0)
                .is_some()
        );
    }

    #[test]
    fn a_low_battery_once() {
        let mut fo = Follower::new(None);
        assert!(
            fo.on_hub(
                "power.battery",
                &f(json!({"level": 20, "charging": false})),
                0,
                0
            )
            .is_none()
        );
        assert!(
            fo.on_hub(
                "power.battery",
                &f(json!({"level": 9, "charging": false})),
                0,
                0
            )
            .is_some()
        );
        assert!(
            fo.on_hub(
                "power.battery",
                &f(json!({"level": 8, "charging": false})),
                0,
                10
            )
            .is_none()
        );
        assert!(
            fo.on_hub(
                "power.battery",
                &f(json!({"level": 5, "charging": true})),
                0,
                10
            )
            .is_none()
        );
    }

    #[test]
    fn the_charger_in_and_out_is_logged() {
        let mut fo = Follower::new(Some(17));
        let e = fo.on_hub("power.charger", &f(serde_json::json!({"online": true})), 422_000, 0).unwrap();
        assert_eq!(e.id, id::POWER_SOURCE);
        assert!(e.summary.contains("plugged in"));
        let e = fo.on_hub("power.charger", &f(serde_json::json!({"online": false})), 430_000, 0).unwrap();
        assert!(e.summary.contains("on battery"));
        assert!(fo.on_hub("power.charger", &f(serde_json::json!({})), 431_000, 0).is_none());
    }
}
