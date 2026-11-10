//! A made-up log built from real saved records of the laptop (trimmed, in `fixtures/`), for the app's sample mode, its
//! pictures and the tests. The boots' numbers and times are invented; the records' text is not.

use crate::analyze::{self, Context, Saved};
use crate::event::{Channel, Event, Level};
use crate::pon::Pon;
use crate::providers::{self, id};

pub const PANIC_HEAP: &str = include_str!("../fixtures/panic-heap-boot11.txt");
pub const RESET_VENUS: &str = include_str!("../fixtures/reset-venus.txt");
pub const RESET_STARTUP: &str = include_str!("../fixtures/reset-startup-boot16.txt");
pub const RESET_RELOAD: &str = include_str!("../fixtures/reset-running.txt");

/// The laptop's usual reasons after a reset: hard reset with the charger in, the last power-off by PS_HOLD.
pub const PON_RESET: Pon = Pon {
    on_reason: 0x41,
    warm_reset: 0,
    poff_reason: 0x02,
    off_seq: 0x80,
    fault: 0x0800,
    s3_reset: 0x80,
};
/// The same with the power key pressed to start it.
pub const PON_KEY: Pon = Pon {
    on_reason: 0xc1,
    warm_reset: 0,
    poff_reason: 0x02,
    off_seq: 0x80,
    fault: 0x0800,
    s3_reset: 0x80,
};

const MIN: i64 = 60_000;
const HOUR: i64 = 3_600_000;
const DAY: i64 = 86_400_000;

fn saved(boot: u32, seq: u32, kind: &str) -> Saved {
    Saved {
        slot: (seq % 8) as usize,
        seq,
        boot,
        kind: kind.into(),
        reason: String::new(),
    }
}

/// The sample log, numbered, oldest first, with "today" holding `now_ms`.
pub fn events(now_ms: i64) -> Vec<Event> {
    let today = now_ms - now_ms.rem_euclid(DAY);
    let mut out: Vec<Event> = Vec::new();
    let boot_at = |boot: u32, start: i64, pon: Pon, last: &str, out: &mut Vec<Event>| {
        out.push(analyze::boot_started(
            boot,
            start + 20_000,
            20_000,
            Some("5.13.0-LucinaJuno juno-9672e3f5ecc1".into()),
            Some(pon),
            last,
        ));
    };
    // Five days ago: one reset during a video, then quiet days.
    let d5 = today - 5 * DAY + 14 * HOUR;
    boot_at(2, d5, PON_RESET, "clean", &mut out);
    let ctx = Context {
        next_pon: Some(PON_RESET),
        next_pon_source: Some("boot 3's kernel log".into()),
        boot_start_ms: Some(d5),
        next_boot_start_ms: Some(d5 + 3 * MIN),
    };
    out.push(analyze::saved_event(
        &saved(2, 20, "unexplained reset"),
        RESET_VENUS.as_bytes(),
        &ctx,
    ));
    boot_at(3, d5 + 3 * MIN, PON_RESET, "vanished", &mut out);
    let mut w = Event::new(
        providers::NET,
        id::LINK_STOPPED,
        Level::Warning,
        Channel::System,
        "Wi-Fi stopped receiving while Ethernet carried on; lucina-watchd reconnected it.",
    );
    w.time_ms = Some(today - 3 * DAY + 10 * HOUR);
    w.boot = Some(4);
    out.push(w);
    let mut a = Event::new(
        providers::APPS,
        id::APP_CRASHED,
        Level::Error,
        Channel::Apps,
        "Gallery ended on a segmentation fault (signal 11).",
    );
    a.time_ms = Some(today - 2 * DAY + 16 * HOUR);
    a.boot = Some(5);
    a.fields.insert("app".into(), "gallery".into());
    a.fields.insert("signal".into(), 11.into());
    out.push(a);
    // Today: the morning's resets.
    let t = today + 8 * HOUR;
    boot_at(7, t, PON_RESET, "clean", &mut out);
    let ctx = Context {
        next_pon: Some(PON_RESET),
        next_pon_source: Some("boot 8's kernel log".into()),
        boot_start_ms: Some(t),
        next_boot_start_ms: Some(t + 4 * MIN),
    };
    out.push(analyze::saved_event(
        &saved(7, 41, "unexplained reset"),
        RESET_VENUS.as_bytes(),
        &ctx,
    ));
    let t11 = t + 2 * HOUR;
    boot_at(11, t11, PON_RESET, "clean", &mut out);
    let ctx = Context {
        next_pon: Some(PON_RESET),
        next_pon_source: Some("boot 12's kernel log".into()),
        boot_start_ms: Some(t11),
        next_boot_start_ms: Some(t11 + 92 * MIN),
    };
    out.push(analyze::saved_event(
        &saved(11, 44, "panic"),
        PANIC_HEAP.as_bytes(),
        &ctx,
    ));
    let t13 = t11 + 3 * HOUR;
    boot_at(13, t13, PON_RESET, "clean", &mut out);
    let ctx = Context {
        next_pon: Some(PON_RESET),
        next_pon_source: Some("boot 14's kernel log".into()),
        boot_start_ms: Some(t13),
        next_boot_start_ms: Some(t13 + 8 * MIN),
    };
    out.push(analyze::saved_event(
        &saved(13, 46, "unexplained reset"),
        RESET_RELOAD.as_bytes(),
        &ctx,
    ));
    let t16 = t13 + 2 * HOUR;
    boot_at(16, t16, PON_RESET, "vanished", &mut out);
    let ctx = Context {
        next_pon: Some(PON_KEY),
        next_pon_source: Some("boot 17's kernel log".into()),
        boot_start_ms: Some(t16),
        next_boot_start_ms: Some(t16 + 2 * MIN),
    };
    out.push(analyze::saved_event(
        &saved(16, 48, "unexplained reset"),
        RESET_STARTUP.as_bytes(),
        &ctx,
    ));
    boot_at(17, t16 + 2 * MIN, PON_KEY, "vanished", &mut out);
    for (i, e) in out.iter_mut().enumerate() {
        e.record = i as u64 + 1;
    }
    out.sort_by_key(|e| (e.time_ms, e.record));
    for (i, e) in out.iter_mut().enumerate() {
        e.record = i as u64 + 1;
    }
    out
}
