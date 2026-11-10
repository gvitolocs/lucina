//! From what the machine kept to an event a person can read: a saved crash record, the PMIC's reasons read at the next boot,
//! and the boots' clocks become a summary in words, a likely cause with how sure it is, the evidence (seen or reasoned, and
//! from where), the log lines that matter, and a bucket.
//!
//! Windows' Kernel-Power 41 stops at "the system rebooted without cleanly shutting down first" and three cases (a bug check
//! code, a power-button time, all zeros). Here every rule below adds evidence; the cause is the strongest one, and its
//! confidence says how many independent facts agree.

use serde_json::json;

use crate::bucket;
use crate::event::{Cause, Channel, Confidence, Event, Evidence, Level, LogLine};
use crate::pon::{OffReason, OnBit, Pon};
use crate::providers::{self, id};
use crate::record::{self, Record};

/// `lucina-shell (pid 751)` -> `lucina-shell`: patch 0842 names the program and its pid.
fn program(who: &str) -> String {
    match who.rsplit_once(" (pid ") {
        Some((name, rest)) if rest.ends_with(')') => name.to_string(),
        _ => who.to_string(),
    }
}

/// How long this laptop's firmware takes from a reset to starting the kernel (measured 2026-11-04, crash-recovery.md).
pub const FIRMWARE_RESTART_MS: i64 = 6_000;
/// With every CPU stuck, the firmware resets the board about this long after (measured 2026-11-04, gen 31).
pub const FIRMWARE_STALL_RESET_MS: u64 = 19_000;
/// A boot whose log stops before this was still starting up.
pub const STARTUP_MS: u64 = 15_000;
/// Only heartbeats for this long before the end: the machine was idle.
pub const IDLE_MS: u64 = 5_000;
/// Log lines kept with an event, after folding repeats.
pub const KEPT_LINES: usize = 60;

/// What the boots around a saved record tell.
#[derive(Clone, Debug, Default)]
pub struct Context {
    /// The PMIC's reasons as read by the boot after the record's boot: why that boot started, and how the one before ended.
    pub next_pon: Option<Pon>,
    /// Where `next_pon` was read: "boot 18's kernel log".
    pub next_pon_source: Option<String>,
    /// Wall clock at the start of the record's boot, unix ms, if a boot-started event knows it.
    pub boot_start_ms: Option<i64>,
    /// Wall clock at the start of the next boot.
    pub next_boot_start_ms: Option<i64>,
}

/// A record as `/sys/fs/pstore/status` lists it.
#[derive(Clone, Debug)]
pub struct Saved {
    pub slot: usize,
    pub seq: u32,
    pub boot: u32,
    /// `panic`, `soft lockup`, `hardware watchdog`, `userspace watchdog`, `unexplained reset`, `requested reset`.
    pub kind: String,
    pub reason: String,
}

impl From<&lucina_watchd::Saved> for Saved {
    fn from(s: &lucina_watchd::Saved) -> Saved {
        Saved {
            slot: s.slot,
            seq: s.seq,
            boot: s.boot,
            kind: s.kind.clone(),
            reason: s.reason.clone(),
        }
    }
}

/// The source key of a saved record: the boot counter and the record's sequence number never repeat together.
pub fn saved_key(boot: u32, seq: u32) -> String {
    format!("pstore:boot{boot}:seq{seq}")
}

/// A subsystem's name in words.
pub fn subsystem_words(sub: &str) -> &str {
    match sub {
        "wifi" | "wlfw" | "ath10k" => "Wi-Fi",
        "venus" | "video" => "the video decoder (Venus)",
        "gpu" | "adreno" | "kgsl" | "drm" => "graphics (GPU)",
        "audio" | "adsp" | "q6" => "sound",
        "net" | "eth0" | "rtl8152" | "usbnet" => "the network",
        "usb" | "xhci" | "dwc3" => "USB",
        "ufs" | "ext4" | "disk" => "the disk",
        "power" | "pon" | "sleep" | "lid" => "power management",
        "charge" | "charger" | "chargerd" | "battery" => "the charger",
        "hid" | "i2c-hid" | "touchpad" | "keyboard" => "the keyboard and touchpad",
        "rpmh" | "bcm" | "icc" => "the power votes (RPMh)",
        "kexec" | "reload" => "a reload",
        "wdt" | "watchdog" | "watchd" => "the watchdog",
        "camera" | "uvc" => "the camera",
        "cpufreq" | "idle" | "psci" => "CPU power states",
        other => other,
    }
}

/// Heartbeat lines that say nothing happened: they show the machine was alive, not what it was doing.
fn is_heartbeat(text: &str) -> bool {
    text.contains(": alive at ") || (text.starts_with("net: \"") && text.contains("\": last "))
}

/// The last line that tells what the machine was doing: not a heartbeat, not part of a run of repeats, with a subsystem.
fn last_activity(log: &[LogLine]) -> Option<(usize, &str)> {
    log.iter().enumerate().rev().find_map(|(i, l)| {
        if is_heartbeat(&l.text) || l.mark.as_deref().is_some_and(|m| m.starts_with("repeated")) {
            return None;
        }
        record::subsystem(&l.text).map(|s| (i, s))
    })
}

fn push_mark(log: &mut [LogLine], i: usize, mark: &str) {
    if let Some(l) = log.get_mut(i) {
        l.mark = Some(match l.mark.take() {
            Some(m) if !m.is_empty() => format!("{m}; {mark}"),
            _ => mark.to_string(),
        });
    }
}

fn secs(ms: u64) -> String {
    format!("{}.{:02} s", ms / 1000, (ms % 1000) / 10)
}

/// The kept log: folded repeats, the last `KEPT_LINES`.
fn kept_log(rec: &Record) -> Vec<LogLine> {
    let folded = record::fold_repeats(&rec.log);
    let start = folded.len().saturating_sub(KEPT_LINES);
    folded[start..].to_vec()
}

/// Evidence and a cause from the PMIC's reasons, for any record. Returns (cause, confidence) when the PMIC alone explains
/// the end (a power loss, a held power key, an overheating PMIC).
fn pon_evidence(
    ctx: &Context,
    ev: &mut Vec<Evidence>,
    fields: &mut serde_json::Map<String, serde_json::Value>,
) -> Option<(String, Confidence)> {
    let pon = ctx.next_pon?;
    let source = ctx
        .next_pon_source
        .clone()
        .unwrap_or_else(|| "the next boot's kernel log".into());
    fields.insert(
        "pon".into(),
        json!({
            "on_reason": format!("{:#04x}", pon.on_reason),
            "warm_reset": format!("{:#04x}", pon.warm_reset),
            "poff_reason": format!("{:#04x}", pon.poff_reason),
            "off_seq": format!("{:#04x}", pon.off_seq),
            "fault": format!("{:#06x}", pon.fault),
            "s3_reset": format!("{:#04x}", pon.s3_reset),
            "started_by": pon.on_bits().iter().map(|b| b.name()).collect::<Vec<_>>(),
            "ended_by": pon.off_reasons().iter().map(|r| r.name()).collect::<Vec<_>>(),
            "warm": pon.is_warm(),
        }),
    );
    let mut cause = None;
    if let Some(off) = pon.off_reason() {
        ev.push(Evidence {
            fact: format!(
                "The PMIC says the power-off came from {}: {}.",
                off.name(),
                off.plain()
            ),
            source: format!(
                "PMIC registers 0xC7 = {:#04x}, 0xC5 = {:#04x}, {source}",
                pon.off_seq, pon.poff_reason
            ),
            line: None,
            observed: true,
        });
        cause = match off {
            OffReason::PowerKeyLongHold | OffReason::PowerKeyAndResetLine | OffReason::S3PowerKeyOrResetLine => Some((
                "Someone held the power key until the machine switched off (Windows records this as a long power-button press).".to_string(),
                Confidence::Certain,
            )),
            OffReason::Uvlo => Some(("The supply voltage fell too low: the battery ran empty or the power dipped.".to_string(), Confidence::Likely)),
            OffReason::OverTemperature => Some(("The PMIC overheated and cut the power.".to_string(), Confidence::Likely)),
            OffReason::PmicWatchdog => Some(("The PMIC's own watchdog expired and cut the power.".to_string(), Confidence::Likely)),
            _ => None,
        };
    }
    let on = pon.on_bits();
    if on.contains(&OnBit::Smpl) {
        ev.push(Evidence {
            fact: "The PMIC started the next boot after a momentary loss of power (SMPL).".into(),
            source: format!("PMIC register 0xC0 = {:#04x}, {source}", pon.on_reason),
            line: None,
            observed: true,
        });
        cause = cause.or(Some((
            "Power was lost for a moment.".to_string(),
            Confidence::Likely,
        )));
    }
    if on.contains(&OnBit::PowerKey) {
        ev.push(Evidence {
            fact: "The next boot was started by the power key: someone pressed it, so the machine was off or frozen until then.".into(),
            source: format!("PMIC register 0xC0 = {:#04x} (bit 7, KPDPWR), {source}", pon.on_reason),
            line: None,
            observed: true,
        });
    }
    if on.contains(&OnBit::Cable) {
        ev.push(Evidence {
            fact: "The charger was connected when it came back on.".into(),
            source: format!(
                "PMIC register 0xC0 = {:#04x} (bit 6, CBLPWR), {source}",
                pon.on_reason
            ),
            line: None,
            observed: true,
        });
    }
    let older: Vec<String> = pon
        .older_reasons()
        .iter()
        .map(|o| format!("{} ({})", o.name(), o.plain()))
        .collect();
    if !older.is_empty() {
        ev.push(Evidence {
            fact: format!("The PMIC also still holds older reasons from an earlier power-off, not this one: {}.", older.join("; ")),
            source: format!("PMIC registers 0xC8..0xCA, {source}"),
            line: None,
            observed: true,
        });
    }
    cause
}

/// A rule that knows a subsystem's way of hanging: matched against the lines of the last seconds before the log stopped.
struct Known {
    sub: &'static str,
    /// A word in a line that makes the match stronger.
    needle: &'static str,
    needle_fact: &'static str,
    cause: &'static str,
    next_step: &'static str,
}

const KNOWN: &[Known] = &[
    Known {
        sub: "kexec",
        needle: "entering the new kernel",
        needle_fact: "it was jumping into a newly loaded kernel (a reload)",
        cause: "A reload (kexec) into a new kernel failed: the new kernel never wrote a line, and the firmware reset the machine.",
        next_step: "Reload only from a freshly booted kernel (docs/research/juno-kexec-enomem.md); scripts/juno-reload.sh does it.",
    },
    Known {
        sub: "venus",
        needle: "idle: woken",
        needle_fact: "Venus had just woken from its power-down",
        cause: "The video decoder (Venus) stalled the machine while waking or decoding. All CPUs stop on the stuck bus access, and the firmware resets the board.",
        next_step: "Repeat the video loop with the kernel log streamed live (crash-recovery.md, Venus).",
    },
    Known {
        sub: "wifi",
        needle: "wlfw",
        needle_fact: "the Wi-Fi firmware was being brought up",
        cause: "The Wi-Fi firmware bring-up stalled the machine.",
        next_step: "Compare the Wi-Fi bring-up steps with the Linux driver's order (juno-wifi-port).",
    },
    Known {
        sub: "gpu",
        needle: "resume",
        needle_fact: "the GPU was resuming",
        cause: "The GPU's power-up stalled the machine.",
        next_step: "Check the GPU's runtime suspend and resume path (juno-gpu-runtime-pm).",
    },
    Known {
        sub: "charge",
        needle: "adc",
        needle_fact: "the charger's ADC was being read",
        cause: "A charger register access over I2C stalled the machine.",
        next_step: "Look at the charger's I2C traffic in the last seconds.",
    },
];

/// The event for one saved record.
pub fn saved_event(saved: &Saved, text: &[u8], ctx: &Context) -> Event {
    let rec = record::parse_record(text);
    // Patch 1113: the boot after a vanished one writes its PMIC reasons into the record itself. They stand in when the
    // ingest did not see that boot's kernel log (a record read in later, an imported dump).
    let from_record;
    let ctx = match (
        ctx.next_pon,
        rec.pmic.as_deref().and_then(Pon::from_record_line),
    ) {
        (None, Some(p)) => {
            from_record = Context {
                next_pon: Some(p),
                next_pon_source: Some(format!(
                    "the record's own pmic line, written by boot {}'s kernel",
                    saved.boot + 1
                )),
                ..ctx.clone()
            };
            &from_record
        }
        _ => ctx,
    };
    let mut log = kept_log(&rec);
    let last_ms = record::last_uptime(&log).or(rec.uptime_ms);
    let mut ev: Vec<Evidence> = Vec::new();
    let mut fields = serde_json::Map::new();
    fields.insert("slot".into(), json!(saved.slot));
    fields.insert("seq".into(), json!(saved.seq));
    fields.insert("kind".into(), json!(saved.kind));
    if let Some(ms) = last_ms {
        fields.insert("last_log_uptime_ms".into(), json!(ms));
    }
    if rec.bits_lost {
        fields.insert("bits_lost".into(), json!(true));
        ev.push(Evidence {
            fact: "Some bits of this record were lost in RAM across the reset; a few characters may be wrong.".into(),
            source: "the saved crash log's error correction".into(),
            line: None,
            observed: true,
        });
    }
    // Which subsystems wrote in the last 30 s: what was running.
    let mut active: Vec<&str> = Vec::new();
    if let Some(end) = last_ms {
        for l in &rec.log {
            if l.uptime_ms.is_some_and(|t| t + 30_000 >= end) {
                if let Some(s) = record::subsystem(&l.text) {
                    if !active.contains(&s) {
                        active.push(s);
                    }
                }
            }
        }
    }
    fields.insert("active".into(), json!(active));
    if !log.is_empty() {
        let n = log.len() - 1;
        push_mark(&mut log, n, "last line");
    }

    // The PMIC's facts come after the log's: what the machine was doing first, how it went off second.
    let mut pmic: Vec<Evidence> = Vec::new();
    let pon_cause = pon_evidence(ctx, &mut pmic, &mut fields);
    let (provider, eid, level, summary, cause, next_step, bucket);
    match saved.kind.as_str() {
        "unexplained reset" => {
            provider = providers::KERNEL_POWER;
            eid = id::UNEXPECTED_RESET;
            level = Level::Critical;
            let startup = last_ms.is_some_and(|ms| ms < STARTUP_MS);
            // Idle: only heartbeats for the last seconds. `exact` is false when the kept tail holds nothing else at all, so
            // the quiet began before it.
            // Measured on the whole saved tail, not the folded one: folding keeps the last line of a run only.
            let quiet = last_ms.and_then(|end| {
                let busy = rec
                    .log
                    .iter()
                    .rev()
                    .find(|l| l.uptime_ms.is_some() && !is_heartbeat(&l.text))
                    .and_then(|l| l.uptime_ms);
                let (from, exact) = match busy {
                    Some(t) => (t, true),
                    None => (rec.log.iter().find_map(|l| l.uptime_ms)?, false),
                };
                Some((end.saturating_sub(from), exact))
            });
            let idle = !startup && quiet.is_some_and(|(ms, _)| ms >= IDLE_MS);
            let phase = if startup {
                "during startup"
            } else if idle {
                "while idle"
            } else {
                "while running"
            };
            if let Some(ms) = last_ms {
                ev.push(Evidence {
                    fact: format!(
                        "The kernel log stops at {} of uptime{}.",
                        secs(ms),
                        if startup {
                            ", so the machine was still starting up"
                        } else {
                            ""
                        }
                    ),
                    source: format!(
                        "saved crash log, record {} (slot {})",
                        saved.seq, saved.slot
                    ),
                    line: Some(log.len().saturating_sub(1)),
                    observed: true,
                });
            }
            if let Some(i) = log
                .iter()
                .rposition(|l| l.mark.as_deref().is_some_and(|m| m.starts_with("repeated")))
            {
                if i + 1 == log.len() && !is_heartbeat(&log[i].text) {
                    ev.push(Evidence {
                        fact: format!("The last line was logged over and over ({}); it filled the saved tail but is not itself a fault.", log[i].mark.clone().unwrap_or_default()),
                        source: format!("saved crash log, record {}", saved.seq),
                        line: Some(i),
                        observed: true,
                    });
                }
            }
            // Patch 0842's line: a program asked for a restart, power-off or reload, and the boot still did not end cleanly.
            if let Some((i, who, cmd)) = log.iter().enumerate().rev().find_map(|(i, l)| {
                let rest = l.text.strip_prefix("[kernel] ")?;
                let (who, cmd) = rest.split_once(" asks for ")?;
                Some((i, program(who), cmd.trim().to_string()))
            }) {
                push_mark(&mut log, i, "asked to end the boot");
                ev.push(Evidence {
                    fact: format!(
                        "{who} had asked for {} just before, but the boot never finished ending.",
                        cmd.to_lowercase()
                    ),
                    source: format!("saved crash log, record {}", saved.seq),
                    line: Some(i),
                    observed: true,
                });
            }
            let activity = last_activity(&log).map(|(i, s)| (i, s.to_string()));
            let mut sub_cause = None;
            if let Some((i, sub)) = &activity {
                push_mark(&mut log, *i, "last activity");
                let at = log[*i]
                    .uptime_ms
                    .map(|t| format!(" at {}", secs(t)))
                    .unwrap_or_default();
                ev.push(Evidence {
                    fact: format!(
                        "The last thing it logged doing was {}{at}: \"{}\".",
                        subsystem_words(sub),
                        log[*i].text.chars().take(120).collect::<String>()
                    ),
                    source: format!("saved crash log, record {}", saved.seq),
                    line: Some(*i),
                    observed: true,
                });
                let tail_from = last_ms.map_or(0, |e| e.saturating_sub(3_000));
                for k in KNOWN.iter().filter(|k| {
                    record::subsystem(&log[*i].text)
                        .is_some_and(|s| s == k.sub || s.starts_with(k.sub))
                }) {
                    let hit = log.iter().enumerate().rev().find(|(_, l)| {
                        l.uptime_ms.is_none_or(|t| t >= tail_from)
                            && l.text.contains(k.needle)
                            && l.text.contains(k.sub)
                    });
                    // A known way of hanging counts only with its own line in the last seconds: a subsystem that merely
                    // spoke last, and then went quiet, proves nothing.
                    if let Some((j, _)) = hit {
                        push_mark(&mut log, j, k.needle_fact);
                        ev.push(Evidence {
                            fact: format!("In the last 3 s of the log, {}.", k.needle_fact),
                            source: format!("saved crash log, record {}", saved.seq),
                            line: Some(j),
                            observed: true,
                        });
                        sub_cause = Some((
                            k.cause.to_string(),
                            Confidence::Likely,
                            k.next_step.to_string(),
                        ));
                    }
                    break;
                }
            }
            if let (true, Some((ms, exact)), Some(end)) = (idle, quiet, last_ms) {
                let mut beats: Vec<&str> = Vec::new();
                for l in log
                    .iter()
                    .filter(|l| l.uptime_ms.is_some_and(|t| t + ms >= end) && is_heartbeat(&l.text))
                {
                    if let Some(s) = record::subsystem(&l.text) {
                        let w = subsystem_words(s);
                        if !beats.contains(&w) {
                            beats.push(w);
                        }
                    }
                }
                ev.push(Evidence {
                    fact: format!(
                        "For {}{} before the end it logged only heartbeats ({}): nothing was running, and it was still alive at {}.",
                        if exact { "the last " } else { "at least the last " },
                        secs(ms),
                        if beats.is_empty() { "periodic lines".to_string() } else { beats.join(", ") },
                        secs(end)
                    ),
                    source: format!("saved crash log, record {}", saved.seq),
                    line: Some(log.len().saturating_sub(1)),
                    observed: true,
                });
            }
            let ps_hold = ctx.next_pon.and_then(|p| p.off_reason()) == Some(OffReason::PsHold);
            let key_restart = ctx
                .next_pon
                .is_some_and(|p| p.on_bits().contains(&OnBit::PowerKey));
            if ps_hold && !key_restart {
                if let Some(ms) = last_ms {
                    ev.push(Evidence {
                        fact: format!(
                            "With every CPU stuck, this laptop's firmware resets it about {} s later (measured 2026-11-04), so the hang likely began right after the last line, around {}.",
                            FIRMWARE_STALL_RESET_MS / 1000,
                            secs(ms)
                        ),
                        source: "crash-recovery.md, measured on gen 31".into(),
                        line: None,
                        observed: false,
                    });
                }
            }
            let what = activity
                .as_ref()
                .map(|(_, s)| subsystem_words(s).to_string());
            summary = match (&what, last_ms) {
                (Some(w), Some(ms)) => format!(
                    "Juno reset itself without a crash report {phase}, {} in; the last activity was {w}.",
                    secs(ms)
                ),
                (None, Some(ms)) => format!(
                    "Juno reset itself without a crash report {phase}, {} in.",
                    secs(ms)
                ),
                _ => "Juno reset itself without a crash report.".into(),
            };
            let (c, conf, step) = match (pon_cause, sub_cause) {
                (Some((c, conf)), _) => (c, conf, None),
                (None, Some((c, conf, step))) => (c, conf, Some(step)),
                (None, None) if idle => (
                    "A hang while idle: every CPU stopped between two heartbeats with nothing running, and the firmware reset the machine. The log does not show which part hung; what powers down when idle (Venus, the GPU, CPU idle states, bus votes) is where to look.".to_string(),
                    Confidence::Possible,
                    Some("Stream /dev/kmsg live while the laptop sits idle, to catch the last line before the stall.".to_string()),
                ),
                (None, None) if ps_hold => (
                    "A hang with every CPU stuck (interrupts off or a stuck bus access), ended by the firmware's reset; the log does not show which part hung.".to_string(),
                    Confidence::Possible,
                    Some("Stream /dev/kmsg live during the next try, to catch the last line before the stall.".to_string()),
                ),
                (None, None) => ("Unknown: nothing recorded why.".to_string(), Confidence::Unknown, None),
            };
            cause = Some(Cause {
                text: c,
                confidence: conf,
            });
            next_step = step;
            let act = what.clone().unwrap_or_else(|| "unknown".into());
            bucket = Some(bucket::make(
                &[
                    ("kind", "unexplained reset".into()),
                    ("phase", phase.into()),
                    (
                        "last_activity",
                        activity.map(|(_, s)| s).unwrap_or_else(|| "none".into()),
                    ),
                ],
                format!("unexplained reset {phase} · last activity: {act}"),
            ));
        }
        // Patch 1057: the modem DSP failed and the boot went on. An event inside a boot, not how the boot ended, so the next
        // boot's PMIC reasons say nothing about it.
        "modem" => {
            provider = providers::KERNEL_LIVE;
            eid = id::MODEM_FAILED;
            level = Level::Error;
            fields.remove("pon");
            let how = match rec.modem_cause.as_deref() {
                Some(c) if c.starts_with("watchdog") => "its watchdog fired",
                Some(c) if c.starts_with("fatal") => "it raised its fatal error bit",
                _ => "it failed",
            };
            let reason = rec.reason.trim().to_string();
            let at = last_ms
                .map(|ms| format!(", {} in", secs(ms)))
                .unwrap_or_default();
            summary =
                format!("The modem DSP, which runs Wi-Fi, failed{at}: {how}. Juno kept running.");
            let src = format!(
                "saved crash log, record {} (slot {})",
                saved.seq, saved.slot
            );
            if let Some(c) = &rec.modem_cause {
                fields.insert("modem_cause".into(), json!(c));
                ev.push(Evidence {
                    fact: format!("The kernel saw the modem fail by its {c}."),
                    source: src.clone(),
                    line: None,
                    observed: true,
                });
            }
            if !reason.is_empty() {
                fields.insert("modem_reason".into(), json!(reason));
                ev.push(Evidence {
                    fact: format!("The modem gave its own reason: \"{reason}\"."),
                    source: "the modem's crash reason in shared memory (SMEM item 421)".into(),
                    line: None,
                    observed: true,
                });
            }
            if let Some((i, sub)) = last_activity(&log).map(|(i, s)| (i, s.to_string())) {
                push_mark(&mut log, i, "last activity");
                ev.push(Evidence {
                    fact: format!(
                        "Just before, the kernel logged {}: \"{}\".",
                        subsystem_words(&sub),
                        log[i].text.chars().take(120).collect::<String>()
                    ),
                    source: src.clone(),
                    line: Some(i),
                    observed: true,
                });
            }
            ev.push(Evidence {
                fact: "A Wi-Fi driver whose modem died cannot start again by itself: Wi-Fi stays off until the next restart.".into(),
                source: "crash-recovery.md, lucina-watchd and more than one link".into(),
                line: None,
                observed: false,
            });
            cause = Some(if reason.is_empty() {
                Cause {
                    text: format!(
                        "The modem firmware failed ({how}) and gave no reason of its own."
                    ),
                    confidence: Confidence::Possible,
                }
            } else {
                Cause {
                    text: format!("The modem firmware failed and said why: {reason}"),
                    confidence: Confidence::Certain,
                }
            });
            next_step = Some("Restart Juno to bring Wi-Fi back; compare the modem's reason with what Linux's remoteproc (qcom_q6v5) logs for the same firmware.".into());
            let norm = bucket::normalize(&reason);
            bucket = Some(bucket::make(
                &[
                    ("kind", "modem".into()),
                    ("cause", rec.modem_cause.clone().unwrap_or_default()),
                    ("reason", norm.clone()),
                ],
                format!(
                    "modem DSP failed: {}",
                    if norm.is_empty() {
                        how.to_string()
                    } else {
                        norm
                    }
                ),
            ));
        }
        "requested reset" => {
            provider = providers::KERNEL_POWER;
            eid = id::RESTART_REQUESTED;
            level = Level::Warning;
            summary = format!("A program restarted Juno: {}.", rec.reason.trim());
            cause = Some(Cause {
                text: format!("Asked for by a program, which said: {}", rec.reason.trim()),
                confidence: Confidence::Certain,
            });
            next_step = None;
            bucket = Some(bucket::make(
                &[
                    ("kind", "requested reset".into()),
                    ("reason", bucket::normalize(&rec.reason)),
                ],
                format!("restart: {}", bucket::normalize(&rec.reason)),
            ));
        }
        kind => {
            provider = providers::CRASH;
            eid = match kind {
                "soft lockup" => id::SOFT_LOCKUP,
                "hardware watchdog" => id::WATCHDOG_BARK,
                "userspace watchdog" => id::USER_WATCHDOG,
                _ => id::KERNEL_CRASH,
            };
            level = Level::Critical;
            let reason = if rec.reason.is_empty() {
                saved.reason.clone()
            } else {
                rec.reason.clone()
            };
            let first = reason.lines().next().unwrap_or("").trim().to_string();
            let plain = plain_reason(kind, &reason);
            let at = rec
                .uptime_ms
                .map(|ms| format!(", {} in", secs(ms)))
                .unwrap_or_default();
            let cpu = rec.cpu.map(|c| format!(" on CPU {c}")).unwrap_or_default();
            summary = format!("Juno crashed{at}: {plain}");
            // The reason's own line in the log, if it is there.
            if let Some(i) = log
                .iter()
                .rposition(|l| !first.is_empty() && l.text.contains(first.as_str()))
            {
                push_mark(&mut log, i, kind);
                ev.push(Evidence {
                    fact: format!("The kernel stopped with a {kind}{cpu}: \"{first}\"."),
                    source: format!(
                        "saved crash log, record {} (slot {})",
                        saved.seq, saved.slot
                    ),
                    line: Some(i),
                    observed: true,
                });
            } else {
                ev.push(Evidence {
                    fact: format!("The kernel stopped with a {kind}{cpu}: \"{first}\"."),
                    source: format!(
                        "saved crash log, record {} (slot {})",
                        saved.seq, saved.slot
                    ),
                    line: None,
                    observed: true,
                });
            }
            if let Some(f) = &rec.fault {
                ev.push(Evidence {
                    fact: format!("The fault was a {f}."),
                    source: "the exception syndrome (ESR) in the record".into(),
                    line: None,
                    observed: true,
                });
            }
            if let Some((i, sub)) =
                last_activity(&log[..log.len().saturating_sub(1)]).map(|(i, s)| (i, s.to_string()))
            {
                push_mark(&mut log, i, "last activity");
                ev.push(Evidence {
                    fact: format!(
                        "Before that it last logged {}: \"{}\".",
                        subsystem_words(&sub),
                        log[i].text.chars().take(120).collect::<String>()
                    ),
                    source: format!("saved crash log, record {}", saved.seq),
                    line: Some(i),
                    observed: true,
                });
            }
            if let Some(pc) = rec.pc {
                fields.insert("pc".into(), json!(format!("{pc:016x}")));
            }
            fields.insert(
                "backtrace".into(),
                json!(
                    rec.backtrace
                        .iter()
                        .map(|a| format!("{a:016x}"))
                        .collect::<Vec<_>>()
                ),
            );
            if let Some(c) = rec.cpu {
                fields.insert("cpu".into(), json!(c));
            }
            fields.insert("reason".into(), json!(reason));
            cause = Some(Cause {
                text: format!("{} The kernel said: {first}", capitalize(&plain)),
                confidence: Confidence::Certain,
            });
            next_step = Some("Name the backtrace's addresses with scripts/juno-symbolize.sh and this build's ELF.".into());
            let norm = bucket::normalize(&reason);
            bucket = Some(bucket::make(
                &[("kind", kind.to_string()), ("reason", norm.clone())],
                format!("{kind}: {norm}"),
            ));
        }
    }

    let mut e = Event::new(provider, eid, level, Channel::System, summary);
    e.key = Some(saved_key(saved.boot, saved.seq));
    e.boot = Some(saved.boot);
    e.uptime_ms = last_ms;
    e.build = rec.build.clone();
    e.activity = Some(format!("boot:{}", saved.boot));
    e.cause = cause;
    e.next_step = next_step;
    e.bucket = bucket;
    if saved.kind != "modem" {
        ev.extend(pmic);
    }
    e.evidence = ev;
    e.fields = fields.into_iter().collect();
    e.log = log;
    e.source = Some(format!("/sys/fs/pstore/dmesg-lucina-{}", saved.slot));
    // When: the boot's start plus the last line's uptime; failing that, the next boot's start less the firmware's restart.
    match (ctx.boot_start_ms, last_ms, ctx.next_boot_start_ms) {
        (Some(start), Some(ms), _) => {
            e.time_ms = Some(start + ms as i64);
            e.time_estimated = true;
        }
        (_, _, Some(next)) if saved.kind != "modem" => {
            e.time_ms = Some(next - FIRMWARE_RESTART_MS);
            e.time_estimated = true;
            e.evidence.push(Evidence {
                fact: "The time is the next boot's start less the firmware's 6 s; the machine may have been off for longer.".into(),
                source: "the boots' clocks".into(),
                line: None,
                observed: false,
            });
        }
        _ => {}
    }
    e
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// A crash's reason in words, for the reasons the kernel gives most.
pub fn plain_reason(kind: &str, reason: &str) -> String {
    let first = reason.lines().next().unwrap_or("").trim();
    match kind {
        "soft lockup" => return "a CPU stopped letting other work run (soft lockup).".into(),
        "hardware watchdog" => return "the hardware watchdog was about to reset the machine: the kernel had stopped answering.".into(),
        "userspace watchdog" => return "lucina-watchd stopped answering, so the kernel crashed on purpose to restart.".into(),
        _ => {}
    }
    if first.starts_with("Heap allocation error") {
        let size = reason
            .split("size:")
            .nth(1)
            .and_then(|s| {
                s.split(|c: char| c == ',' || c.is_whitespace())
                    .find(|w| !w.is_empty())
            })
            .and_then(|w| {
                w.strip_prefix("0x").map_or_else(
                    || w.parse::<u64>().ok(),
                    |h| u64::from_str_radix(h, 16).ok(),
                )
            });
        return match size {
            Some(b) if b >= 1024 => format!(
                "the kernel could not get {} KiB of memory in one piece.",
                b / 1024
            ),
            Some(b) => format!("the kernel could not get {b} bytes of memory."),
            None => "the kernel ran out of memory for an allocation.".into(),
        };
    }
    if first.contains("entered unreachable code") {
        return format!(
            "the kernel reached code it held to be impossible ({}).",
            first.rsplit(" at ").next().unwrap_or(first)
        );
    }
    if first.contains("called `Option::unwrap()` on a `None` value")
        || first.contains("called `Result::unwrap()`")
    {
        return format!("the kernel found a value missing that it was sure of ({first}).");
    }
    if first.contains("attempt to") && first.contains("overflow") {
        return format!("a number in the kernel overflowed ({first}).");
    }
    if first.contains("index out of bounds") {
        return format!("the kernel read past the end of a table ({first}).");
    }
    format!("{first}.")
}

/// The boot-started event for the running boot (Kernel-General 12 / 6005), with the PMIC's reasons.
pub fn boot_started(
    boot: u32,
    now_ms: i64,
    uptime_ms: u64,
    build: Option<String>,
    pon: Option<Pon>,
    last_boot: &str,
) -> Event {
    let how = match pon {
        Some(p) => {
            let by: Vec<&str> = p.on_bits().iter().map(|b| b.plain()).collect();
            if by.is_empty() {
                String::new()
            } else {
                format!(" ({})", by.join("; "))
            }
        }
        None => String::new(),
    };
    let mut e = Event::new(
        providers::KERNEL_POWER,
        id::BOOT_STARTED,
        Level::Info,
        Channel::System,
        format!("Juno started, boot {boot}{how}."),
    );
    e.key = Some(format!("boot:{boot}"));
    e.boot = Some(boot);
    e.uptime_ms = Some(0);
    e.time_ms = Some(now_ms - uptime_ms as i64);
    e.build = build;
    e.activity = Some(format!("boot:{boot}"));
    e.fields.insert("last_boot".into(), json!(last_boot));
    if let Some(p) = pon {
        e.fields.insert(
            "pon_raw".into(),
            json!([
                p.on_reason,
                p.warm_reset,
                p.poff_reason,
                p.off_seq,
                p.fault & 0xff,
                p.fault >> 8,
                p.s3_reset
            ]),
        );
        e.fields.insert(
            "started_by".into(),
            json!(p.on_bits().iter().map(|b| b.name()).collect::<Vec<_>>()),
        );
        e.fields.insert("warm".into(), json!(p.is_warm()));
    }
    e
}

/// The PMIC's reasons back from a boot-started event's `pon_raw`.
pub fn pon_of(e: &Event) -> Option<Pon> {
    let raw: Vec<u64> = e
        .fields
        .get("pon_raw")?
        .as_array()?
        .iter()
        .filter_map(|v| v.as_u64())
        .collect();
    if raw.len() != 7 {
        return None;
    }
    Some(Pon {
        on_reason: raw[0] as u8,
        warm_reset: raw[1] as u8,
        poff_reason: raw[2] as u8,
        off_seq: raw[3] as u8,
        fault: (raw[4] as u16) | ((raw[5] as u16) << 8),
        s3_reset: raw[6] as u8,
    })
}

/// The clean end of a boot (Kernel-General 13 / User32 1074 rolled into one), with who asked, from that boot's log.
pub fn clean_end(boot: u32, last_log: &[LogLine], next_boot_start_ms: Option<i64>) -> Event {
    // Patch 0842: "[kernel] <program> (pid <n>) asks for <Command>".
    let asked = last_log.iter().rev().find_map(|l| {
        let rest = l.text.strip_prefix("[kernel] ")?;
        let (who, cmd) = rest.split_once(" asks for ")?;
        Some((program(who), cmd.trim().to_string(), l.uptime_ms))
    });
    let summary = match &asked {
        Some((who, cmd, _)) => format!(
            "Boot {boot} ended cleanly: {who} asked for {}.",
            cmd.to_lowercase()
        ),
        None => format!("Boot {boot} ended cleanly (a restart, power-off or reload)."),
    };
    let mut e = Event::new(
        providers::KERNEL_POWER,
        id::SHUT_DOWN,
        Level::Info,
        Channel::System,
        summary,
    );
    e.key = Some(format!("end:boot{boot}"));
    e.boot = Some(boot);
    e.activity = Some(format!("boot:{boot}"));
    if let Some((who, cmd, at)) = asked {
        e.fields.insert("by".into(), json!(who));
        e.fields.insert("command".into(), json!(cmd));
        e.uptime_ms = at;
    }
    if let Some(next) = next_boot_start_ms {
        e.time_ms = Some(next - FIRMWARE_RESTART_MS);
        e.time_estimated = true;
    }
    e
}

#[cfg(test)]
mod tests;
