//! Juno's event log and reliability report. Design and the Windows study: `docs/research/juno-event-log.md`.
//!
//! - [`event`]: one typed event (provider, ID, level, channel, fields) with a plain summary, likely cause and evidence.
//! - [`providers`]: every provider and ID, with the Windows event it stands for.
//! - [`pon`]: the PMIC's power-on and power-off reasons (patch 0841's `power: PON reasons` line), decoded.
//! - [`record`]: a saved crash record's text (`/sys/fs/pstore/dmesg-lucina-<slot>`) and kernel log lines, parsed.
//! - [`analyze`]: a saved record plus the next boot's PON reasons into an event: summary, likely cause, evidence, bucket.
//! - [`bucket`]: crash signatures, so that repeats group together.
//! - [`store`]: the log on /data: append-only JSON lines, numbered records, retention, file and directory fsync.
//! - [`ingest`]: what a boot reads in: the saved crash log and the PMIC's reasons, once.
//! - [`export`]: the log in words and in one JSON, for people and agents (CLI, lucina-devd's `reliability` tool).
//! - [`follow`]: the running system's events and kernel errors as they happen, with repeats folded.
//! - [`rules`]: the PMIC reason choice, the warning gate and retention's drop count, Verus-checked as shipped.
//! - [`sample`]: a made-up log from real records, for the app and tests.
//! - [`timeline`]: Reliability Monitor's days, categories and the stability index, with its formula shown.

pub mod analyze;
pub mod bucket;
pub mod event;
pub mod export;
pub mod follow;
pub mod ingest;
pub mod pon;
pub mod providers;
pub mod record;
pub mod rules;
pub mod sample;
pub mod store;
pub mod timeline;

pub use event::{Bucket, Category, Cause, Channel, Confidence, Event, Evidence, Level, LogLine};
