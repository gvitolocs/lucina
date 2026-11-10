//! One event: what Windows' event record holds (provider, ID, level, channel, time, typed fields, an activity that ties
//! events of one incident together), plus what Windows leaves to the reader: a plain one-line summary, a likely cause with
//! how sure it is, the evidence behind it (each item saying whether it was seen or inferred, and from where), the kernel log
//! lines it points at, and the crash bucket that groups its repeats. Every event is self-describing: the summary and the
//! field names are stored with it, so an old log reads the same on any build (Windows needs the provider's message DLL).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Windows' numbers (winmeta.xml), so that a reader who knows Event Viewer reads ours the same way.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    /// The system stopped: a kernel crash, an unexpected reset.
    Critical = 1,
    /// Something failed and did not recover by itself: an app crashed, an install failed.
    Error = 2,
    /// Something failed and recovered, or may fail soon: a link stopped, a firmware restarted, the battery is low.
    Warning = 3,
    /// A normal step worth a line on the timeline: a boot, a clean shutdown, an install.
    Info = 4,
    /// Diagnostics, never shown by default.
    Verbose = 5,
}

impl Level {
    pub fn name(self) -> &'static str {
        match self {
            Level::Critical => "critical",
            Level::Error => "error",
            Level::Warning => "warning",
            Level::Info => "info",
            Level::Verbose => "verbose",
        }
    }

    pub fn parse(s: &str) -> Option<Level> {
        Some(match s {
            "critical" | "1" => Level::Critical,
            "error" | "2" => Level::Error,
            "warning" | "warn" | "3" => Level::Warning,
            "info" | "information" | "4" => Level::Info,
            "verbose" | "5" => Level::Verbose,
            _ => return None,
        })
    }
}

/// Windows' System and Application logs, and Setup for installs. Each has its own size and age limit (`store::Retention`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    System,
    Apps,
    Setup,
}

impl Channel {
    pub fn name(self) -> &'static str {
        match self {
            Channel::System => "system",
            Channel::Apps => "apps",
            Channel::Setup => "setup",
        }
    }
}

/// How sure a likely cause is. Windows never says; we always do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    /// The system said why itself: a panic's reason, a program that asked for the restart.
    Certain,
    /// Two or more independent facts point the same way, or one fact that has meant this before (a known bucket).
    Likely,
    /// One fact points this way; others may too.
    Possible,
    /// Nothing points anywhere: the summary says what is known and no more.
    Unknown,
}

impl Confidence {
    pub fn name(self) -> &'static str {
        match self {
            Confidence::Certain => "certain",
            Confidence::Likely => "likely",
            Confidence::Possible => "possible",
            Confidence::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cause {
    pub text: String,
    pub confidence: Confidence,
}

/// One fact behind a cause.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    /// In words: "the kernel log stops at 118.04 s, 1.2 s after Venus woke".
    pub fact: String,
    /// Where it came from: "saved crash log, record 3", "PMIC register 0xC0 = 0xC1", "this boot's /dev/kmsg".
    pub source: String,
    /// Index into the event's `log`, for the line it points at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    /// Seen directly (true) or reasoned from what was seen (false).
    pub observed: bool,
}

/// A kernel log line kept with the event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogLine {
    /// Uptime of the boot that logged it, if the line had a time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uptime_ms: Option<u64>,
    pub text: String,
    /// Why the line matters, when it does: "last line", "Venus woke", "panic".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mark: Option<String>,
}

/// What groups repeats of one problem: Windows Error Reporting's bucket, with a name a person can read and its inputs named.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bucket {
    /// 16 hex digits, stable across builds for the same signature.
    pub id: String,
    /// "unexplained reset · video decoding (venus)".
    pub label: String,
    /// The named inputs of the signature, in order.
    pub inputs: BTreeMap<String, String>,
}

/// One event. `record` is given by the store; everything else by whoever made the event.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Event {
    /// Numbered by the store from 1, never reused, kept across rotation (Windows' EventRecordID).
    #[serde(default)]
    pub record: u64,
    /// What makes this event unique among its source's, so that reading the same source twice adds nothing:
    /// `pstore:boot17:seq42`, `pon:boot18`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    pub provider: String,
    pub id: u32,
    pub level: Level,
    pub channel: Channel,
    /// Unix milliseconds, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_ms: Option<i64>,
    /// The time was worked out (a reset's time from its boot's start and last log line), not read from a clock.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub time_estimated: bool,
    /// The saved log's boot counter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boot: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uptime_ms: Option<u64>,
    /// The kernel build that was running (`build:` of a crash record, or `/proc/version`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<String>,
    /// Events of one incident share it (Windows' ActivityID): `boot:17`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<String>,
    /// One line in plain words.
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<Cause>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<Evidence>,
    /// What to do about it, if anything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_step: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bucket: Option<Bucket>,
    /// Typed values (Windows' EventData), by name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fields: BTreeMap<String, Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub log: Vec<LogLine>,
    /// Where the raw text is, while it still is: `/sys/fs/pstore/dmesg-lucina-3`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

impl Event {
    /// An event with what every one needs; the rest is filled in by the caller.
    pub fn new(
        provider: &str,
        id: u32,
        level: Level,
        channel: Channel,
        summary: impl Into<String>,
    ) -> Event {
        Event {
            record: 0,
            key: None,
            provider: provider.to_string(),
            id,
            level,
            channel,
            time_ms: None,
            time_estimated: false,
            boot: None,
            uptime_ms: None,
            build: None,
            activity: None,
            summary: summary.into(),
            cause: None,
            evidence: Vec::new(),
            next_step: None,
            bucket: None,
            fields: BTreeMap::new(),
            log: Vec::new(),
            source: None,
        }
    }

    pub fn field(mut self, name: &str, value: impl Into<Value>) -> Event {
        self.fields.insert(name.to_string(), value.into());
        self
    }

    /// Which part of the timeline it counts in.
    pub fn category(&self) -> Category {
        match (self.level, self.channel) {
            (Level::Critical, _) => Category::JunoFailure,
            (Level::Error, Channel::Apps) => Category::AppFailure,
            (Level::Error, _) => Category::JunoFailure,
            (Level::Warning, _) => Category::Warning,
            _ => Category::Info,
        }
    }
}

/// Reliability Monitor's rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    /// The system crashed, reset or failed to install: Windows failures.
    JunoFailure,
    /// An app crashed: application failures.
    AppFailure,
    Warning,
    Info,
}

impl Category {
    pub fn name(self) -> &'static str {
        match self {
            Category::JunoFailure => "Juno failures",
            Category::AppFailure => "App failures",
            Category::Warning => "Warnings",
            Category::Info => "Information",
        }
    }
}
