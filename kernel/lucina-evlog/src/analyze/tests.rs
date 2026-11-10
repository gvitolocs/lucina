use super::*;
use crate::sample::{PANIC_HEAP, PON_KEY, PON_RESET, RESET_RELOAD, RESET_STARTUP, RESET_VENUS};

fn saved(kind: &str) -> Saved {
    Saved {
        slot: 3,
        seq: 48,
        boot: 16,
        kind: kind.into(),
        reason: String::new(),
    }
}

fn ctx(pon: Pon) -> Context {
    Context {
        next_pon: Some(pon),
        next_pon_source: Some("boot 17's kernel log".into()),
        boot_start_ms: Some(1_000_000),
        next_boot_start_ms: Some(2_000_000),
    }
}

#[test]
fn a_reset_during_startup_names_wifi_and_the_power_key() {
    let e = saved_event(
        &saved("unexplained reset"),
        RESET_STARTUP.as_bytes(),
        &ctx(PON_KEY),
    );
    assert_eq!(e.id, id::UNEXPECTED_RESET);
    assert_eq!(e.level, Level::Critical);
    assert!(e.summary.contains("during startup"), "{}", e.summary);
    assert!(e.summary.contains("Wi-Fi"), "{}", e.summary);
    assert_eq!(e.key.as_deref(), Some("pstore:boot16:seq48"));
    let facts: Vec<&str> = e.evidence.iter().map(|v| v.fact.as_str()).collect();
    assert!(facts.iter().any(|f| f.contains("power key")), "{facts:#?}");
    assert!(
        facts.iter().any(|f| f.contains("over and over")),
        "{facts:#?}"
    );
    assert!(facts.iter().any(|f| f.contains("PS_HOLD")), "{facts:#?}");
    // The spam is one line, and the evidence points at real lines.
    assert!(
        e.log
            .iter()
            .any(|l| l.mark.as_deref().is_some_and(|m| m.contains("repeated")))
    );
    for v in &e.evidence {
        if let Some(i) = v.line {
            assert!(i < e.log.len());
        }
    }
    let c = e.cause.unwrap();
    assert_eq!(c.confidence, Confidence::Likely, "{}", c.text);
    assert!(c.text.contains("Wi-Fi"), "{}", c.text);
    assert_eq!(e.time_ms, Some(1_000_000 + e.uptime_ms.unwrap() as i64));
    assert!(e.time_estimated);
    assert!(e.bucket.unwrap().label.contains("during startup"));
}

#[test]
fn a_reset_while_decoding_video() {
    let e = saved_event(
        &saved("unexplained reset"),
        RESET_VENUS.as_bytes(),
        &ctx(PON_RESET),
    );
    assert!(e.summary.contains("while running"), "{}", e.summary);
    assert!(e.summary.contains("Venus"), "{}", e.summary);
    // Venus woke within the last 3 s: its own line makes the known pattern likely.
    let c = e.cause.as_ref().unwrap();
    assert_eq!(c.confidence, Confidence::Likely, "{}", c.text);
    assert!(c.text.contains("Venus"), "{}", c.text);
    assert!(
        e.evidence
            .iter()
            .any(|v| !v.observed && v.fact.contains("19 s")),
        "{:#?}",
        e.evidence
    );
    assert_eq!(
        e.fields["active"]
            .as_array()
            .map(|a| a.iter().any(|s| s == "venus")),
        Some(true)
    );
}

#[test]
fn a_reset_while_idle() {
    let mut t =
        String::from("The machine reset with no crash report\nreason: x\nlast kernel log:\n");
    t.push_str("[   23.887000] wifi: net: registering wlan0\n");
    for k in 0..7 {
        t.push_str(&format!(
            "[   {}.071000] wifi: alive at {} ms, 1 loops\n",
            24 + 2 * k,
            (24 + 2 * k) * 1000
        ));
        if k == 2 {
            t.push_str("[   29.829000] audio: alive at 29790 ms, 3722 loops\n");
        }
    }
    let e = saved_event(&saved("unexplained reset"), t.as_bytes(), &ctx(PON_RESET));
    assert!(e.summary.contains("while idle"), "{}", e.summary);
    let c = e.cause.as_ref().unwrap();
    assert_eq!(c.confidence, Confidence::Possible);
    assert!(c.text.starts_with("A hang while idle"), "{}", c.text);
    let quiet = e
        .evidence
        .iter()
        .find(|v| v.fact.contains("only heartbeats"))
        .expect("the quiet is evidence");
    assert!(quiet.fact.contains("the last 12.18 s"), "{}", quiet.fact);
    assert!(quiet.fact.contains("Wi-Fi, sound"), "{}", quiet.fact);
    assert!(
        e.bucket
            .unwrap()
            .label
            .starts_with("unexplained reset while idle")
    );
    // Only heartbeats in the whole tail: "at least".
    let only: String = (0..8)
        .map(|k| format!("[ {}.000000] wifi: alive at 1 ms\n", 6000 + 2 * k))
        .collect();
    let t2 = format!("The machine reset with no crash report\nreason: x\nlast kernel log:\n{only}");
    let e2 = saved_event(
        &saved("unexplained reset"),
        t2.as_bytes(),
        &Context::default(),
    );
    assert!(
        e2.evidence
            .iter()
            .any(|v| v.fact.contains("at least the last 14.00 s")),
        "{:#?}",
        e2.evidence
    );
}

#[test]
fn a_reset_during_a_reload() {
    let e = saved_event(
        &saved("unexplained reset"),
        RESET_RELOAD.as_bytes(),
        &ctx(PON_RESET),
    );
    let c = e.cause.as_ref().unwrap();
    assert!(c.text.contains("reload"), "{}", c.text);
    assert_eq!(c.confidence, Confidence::Likely);
    assert!(
        e.evidence
            .iter()
            .any(|v| v.fact.contains("lucina-reload had asked for kexec")),
        "{:#?}",
        e.evidence
    );
}

#[test]
fn a_panic_says_what_in_words_and_buckets_across_builds() {
    let e = saved_event(&saved("panic"), PANIC_HEAP.as_bytes(), &ctx(PON_RESET));
    assert_eq!(e.provider, providers::CRASH);
    assert_eq!(e.id, id::KERNEL_CRASH);
    assert!(e.summary.contains("256 KiB"), "{}", e.summary);
    assert_eq!(e.cause.as_ref().unwrap().confidence, Confidence::Certain);
    assert_eq!(
        e.build.as_deref(),
        Some("5.13.0-LucinaJuno juno-9672e3f5ecc1")
    );
    assert!(
        e.log
            .iter()
            .any(|l| l.mark.as_deref().is_some_and(|m| m.contains("panic")))
    );
    let other = PANIC_HEAP
        .replace("juno-9672e3f5ecc1", "juno-000000000000")
        .replace("ffffffffa08cfe40", "ffffffffa0000000");
    let f = saved_event(&saved("panic"), other.as_bytes(), &ctx(PON_RESET));
    assert_eq!(e.bucket.as_ref().unwrap().id, f.bucket.as_ref().unwrap().id);
}

#[test]
fn a_held_power_key_is_certain() {
    let pon = Pon {
        on_reason: 0x80,
        warm_reset: 0,
        poff_reason: 0x80,
        off_seq: 0x80,
        fault: 0,
        s3_reset: 0,
    };
    let e = saved_event(
        &saved("unexplained reset"),
        RESET_VENUS.as_bytes(),
        &ctx(pon),
    );
    let c = e.cause.unwrap();
    assert_eq!(c.confidence, Confidence::Certain);
    assert!(c.text.contains("held the power key"));
}

#[test]
fn without_a_next_boot_nothing_is_invented() {
    let e = saved_event(
        &saved("unexplained reset"),
        b"The machine reset with no crash report\nreason: x\nlast kernel log:\n",
        &Context::default(),
    );
    assert_eq!(e.cause.unwrap().confidence, Confidence::Unknown);
    assert_eq!(e.time_ms, None);
}

#[test]
fn reasons_in_words() {
    assert_eq!(
        plain_reason(
            "panic",
            "Heap allocation error, layout = Layout {\n size: 0x40000,"
        ),
        "the kernel could not get 256 KiB of memory in one piece."
    );
    assert!(
        plain_reason(
            "panic",
            "entered unreachable code at ostd/src/task/mod.rs:210"
        )
        .contains("impossible")
    );
    assert!(plain_reason("soft lockup", "x").contains("soft lockup"));
}

#[test]
fn boot_events_carry_the_pmic() {
    let e = boot_started(17, 10_000, 3_000, None, Some(PON_KEY), "vanished");
    assert_eq!(e.time_ms, Some(7_000));
    assert!(
        e.summary.contains("the power key was pressed"),
        "{}",
        e.summary
    );
    assert_eq!(pon_of(&e), Some(PON_KEY));
    let c = clean_end(
        16,
        &[LogLine {
            uptime_ms: Some(5),
            text: "[kernel] lucina-shell (pid 9) asks for PowerOff".into(),
            mark: None,
        }],
        Some(100_000),
    );
    assert_eq!(
        c.summary,
        "Boot 16 ended cleanly: lucina-shell asked for poweroff."
    );
}

#[test]
fn the_sample_log_has_today_and_a_repeat() {
    let now = 1_794_052_800_000; // 2026-11-07 12:00 UTC
    let ev = crate::sample::events(now);
    assert!(ev.windows(2).all(|w| w[0].record < w[1].record));
    let groups = crate::bucket::groups(&ev);
    assert!(groups.iter().any(|g| g.count >= 2), "{groups:#?}");
    let days = crate::timeline::days(&ev, now + 11 * 3_600_000, 7, 0);
    assert!(days.last().unwrap().juno_failures >= 4);
}

#[test]
fn a_modem_failure_is_an_event_inside_the_boot() {
    let t = "The modem DSP failed\ncause: watchdog interrupt (SPI 266)\nreason: err_qdi.c:702:PD 1 crashed\nlast kernel log:\n[  100.000000] wifi: net: the port is open\n[  101.500000] wifi: alive at 101000 ms, 1 loops\n";
    let e = saved_event(&saved("modem"), t.as_bytes(), &ctx(PON_KEY));
    assert_eq!(
        (e.provider.as_str(), e.id, e.level),
        (providers::KERNEL_LIVE, id::MODEM_FAILED, Level::Error)
    );
    assert!(
        e.summary.contains("Wi-Fi")
            && e.summary.contains("watchdog")
            && e.summary.contains("kept running"),
        "{}",
        e.summary
    );
    let c = e.cause.as_ref().unwrap();
    assert_eq!(c.confidence, Confidence::Certain);
    assert!(c.text.contains("PD 1 crashed"), "{}", c.text);
    // The next boot's power-on says nothing about a boot that went on.
    assert!(
        !e.evidence.iter().any(|v| v.fact.contains("PMIC")),
        "{:#?}",
        e.evidence
    );
    assert!(!e.fields.contains_key("pon"));
    assert_eq!(e.time_ms, Some(1_000_000 + 101_500));
    assert!(
        e.bucket
            .as_ref()
            .unwrap()
            .label
            .starts_with("modem DSP failed: err_qdi.c:#:PD 1 crashed"),
        "{}",
        e.bucket.unwrap().label
    );
    // Without the boot's own start, no time: the next boot's start is not when the modem failed.
    let only_next = Context {
        boot_start_ms: None,
        ..ctx(PON_KEY)
    };
    assert_eq!(
        saved_event(&saved("modem"), t.as_bytes(), &only_next).time_ms,
        None
    );
    // No reason from the modem: possible, and the bucket names how it failed.
    let bare = saved_event(
        &saved("modem"),
        b"The modem DSP failed\ncause: fatal bit (SMP2P)\nreason: \nlast kernel log:\n",
        &Context::default(),
    );
    assert_eq!(
        bare.cause.as_ref().unwrap().confidence,
        Confidence::Possible
    );
    assert_eq!(
        bare.bucket.as_ref().unwrap().label,
        "modem DSP failed: it raised its fatal error bit"
    );
    assert_eq!(bare.category(), crate::event::Category::JunoFailure);
}

#[test]
fn the_records_own_pmic_line_stands_in_for_the_next_boots_log() {
    let t = "The machine reset with no crash report\nreason: nothing recorded a crash\npmic: last power-off by PS_HOLD (x); this boot by hard reset, cold [c0 c1 c2 00 c5 02 c7 80 c8 00 c9 08 ca 80]\nlast kernel log:\n[  100.000000] wifi: net: the port is open\n";
    let e = saved_event(
        &saved("unexplained reset"),
        t.as_bytes(),
        &Context::default(),
    );
    let key = e
        .evidence
        .iter()
        .find(|v| v.fact.starts_with("The next boot was started by the power key"))
        .expect("the power key from the record's line");
    assert!(
        key.source.contains("pmic line, written by boot 17"),
        "{}",
        key.source
    );
    assert!(e.fields.contains_key("pon"));
    // The next boot's own log, when the ingest has it, still wins.
    let e2 = saved_event(&saved("unexplained reset"), t.as_bytes(), &ctx(PON_RESET));
    assert!(
        !e2.evidence.iter().any(|v| v.fact.starts_with("The next boot was started by the power key")),
        "{:#?}",
        e2.evidence
    );
    // An old record without the line: nothing invented.
    let old = saved_event(
        &saved("unexplained reset"),
        RESET_VENUS.as_bytes(),
        &Context::default(),
    );
    assert!(!old.fields.contains_key("pon"));
}
