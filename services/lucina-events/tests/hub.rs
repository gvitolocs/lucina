use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use lucina_events::hub::{Config, Hub, Refusal, SourceKey};
use lucina_events::rules;
use lucina_events::wire::{Field, Filter, Source};
use serde_json::{json, Value};

fn test_hub(cap: u64, burst: u32) -> (Arc<Hub>, Arc<AtomicU64>) {
    let t = Arc::new(AtomicU64::new(1000));
    let cfg = Config {
        cap,
        max_read: 64,
        every_ms: 100,
        burst,
        socket: "/tmp/lucina-test-unused.sock".into(),
        read_socket: "/tmp/lucina-test-unused-read.sock".into(),
        kernel: None,
    };
    let clock = {
        let t = t.clone();
        move || t.load(Ordering::Relaxed)
    };
    (Arc::new(Hub::new(cfg, clock)), t)
}

fn f(n: &str, v: Value, vis: u8) -> Field {
    Field {
        n: n.to_string(),
        v,
        vis,
    }
}

fn key() -> SourceKey {
    (Source::App, 1)
}

fn accept(h: &Hub, src_seq: u64, kind: &str) -> Result<(), Refusal> {
    h.accept(
        Source::App,
        Some("player"),
        rules::INTROSPECT_ALL,
        key(),
        src_seq,
        kind,
        None,
        vec![],
    )
}

fn advance_after(t: Arc<AtomicU64>, ms: u64, delta: u64) {
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(ms));
        t.fetch_add(delta, Ordering::Relaxed);
    });
}

#[test]
fn ordering() {
    let (h, _t) = test_hub(16, 20);
    for i in 0..3 {
        assert_eq!(accept(&h, i, "media.step"), Ok(()));
    }
    let b = h.read(0, &Filter::default(), None);
    assert_eq!(b.events.len(), 3);
    assert_eq!(b.next, 3);
    assert!(!b.reset);
    for (i, e) in b.events.iter().enumerate() {
        assert_eq!(e.seq, i as u64);
        assert_eq!(e.src_seq, i as u64);
    }
}

#[test]
fn drop_oldest_and_lost() {
    let (h, _t) = test_hub(4, 100);
    for i in 0..6 {
        assert_eq!(accept(&h, i, "media.step"), Ok(()));
    }
    let b = h.read(0, &Filter::default(), None);
    assert_eq!(b.lost, 2);
    assert_eq!(b.events.len(), 4);
    assert_eq!(b.events[0].seq, 2);
    assert_eq!(b.next, 6);
    let stats = h.stats();
    assert_eq!(stats["dropped"], json!(2));
}

#[test]
fn reset_for_future_cursor() {
    let (h, _t) = test_hub(4, 100);
    accept(&h, 0, "media.step").unwrap();
    let b = h.read(100, &Filter::default(), None);
    assert!(b.reset);
    assert_eq!(b.lost, 0);
    assert_eq!(b.events.len(), 1);
    assert_eq!(b.next, 1);
}

#[test]
fn stale_refused() {
    let (h, _t) = test_hub(16, 100);
    accept(&h, 0, "media.step").unwrap();
    assert_eq!(accept(&h, 0, "media.step"), Err(Refusal::Stale));
    assert_eq!(accept(&h, 5, "media.step"), Ok(())); // a gap, not stale
    assert_eq!(accept(&h, 5, "media.step"), Err(Refusal::Stale));
    assert_eq!(h.stats()["stale"], json!(2));
}

#[test]
fn gap_inserts_lost_event() {
    let (h, _t) = test_hub(16, 100);
    accept(&h, 0, "media.step").unwrap();
    assert_eq!(accept(&h, 3, "media.step"), Ok(()));
    let b = h.read(0, &Filter::default(), None);
    assert_eq!(b.events.len(), 3);
    let lost = &b.events[1];
    assert_eq!(lost.kind, "events.lost");
    assert_eq!(lost.fields["count"], json!(2));
    assert_eq!(lost.fields["reason"], json!("gap"));
    assert_eq!(b.events[2].src_seq, 3);
    assert_eq!(h.stats()["gaps"], json!(2));
}

#[test]
fn flood_guard() {
    let (h, t) = test_hub(16, 3);
    for i in 0..3 {
        assert_eq!(accept(&h, i, "media.step"), Ok(()));
    }
    assert_eq!(accept(&h, 3, "media.step"), Err(Refusal::Flooded));
    // time passes and another event comes: one flood note appears, then the event
    t.store(2500, Ordering::Relaxed);
    assert_eq!(accept(&h, 4, "media.step"), Ok(()));
    let b = h.read(0, &Filter::default(), None);
    let flood = b
        .events
        .iter()
        .find(|e| e.kind == "events.lost" && e.fields["reason"] == json!("flood"))
        .expect("a flood note");
    assert_eq!(flood.fields["count"], json!(1));
    assert_eq!(h.stats()["flooded"], json!(1));
}

#[test]
fn private_and_secret_fields_never_leave() {
    let (h, _t) = test_hub(16, 100);
    h.accept(
        Source::App,
        Some("player"),
        rules::INTROSPECT_ALL,
        key(),
        0,
        "note.changed",
        None,
        vec![
            f("title", json!("ok"), rules::VIS_PUBLIC),
            f("text", json!("the note body"), rules::VIS_PRIVATE),
            f("token", json!("abc"), rules::VIS_SECRET),
        ],
    )
    .unwrap();
    let b = h.read(0, &Filter::default(), None);
    let e = &b.events[0];
    assert_eq!(e.fields.len(), 1);
    assert_eq!(e.fields["title"], json!("ok"));
    assert!(e.fields.get("text").is_none());
    assert!(e.fields.get("token").is_none());
}

#[test]
fn owner_input_has_no_fields() {
    let (h, _t) = test_hub(16, 100);
    h.accept(
        Source::App,
        Some("player"),
        rules::INTROSPECT_ALL,
        key(),
        0,
        "owner.input",
        None,
        vec![f("key", json!("x"), rules::VIS_PUBLIC)],
    )
    .unwrap();
    let b = h.read(0, &Filter::default(), None);
    assert_eq!(b.events[0].kind, "owner.input");
    assert!(b.events[0].fields.is_empty());
}

#[test]
fn introspect_none_not_recorded_but_shell_event_is() {
    let (h, _t) = test_hub(16, 100);
    let r = h.accept(
        Source::App,
        Some("hidden"),
        rules::INTROSPECT_NONE,
        (Source::App, 9),
        0,
        "media.ended",
        None,
        vec![],
    );
    assert_eq!(r, Ok(()));
    assert!(h.read(0, &Filter::default(), None).events.is_empty());
    h.accept(
        Source::Shell,
        Some("hidden"),
        rules::INTROSPECT_ALL,
        (Source::Shell, 2),
        0,
        "app.front",
        Some("hidden"),
        vec![],
    )
    .unwrap();
    let b = h.read(0, &Filter::default(), None);
    assert_eq!(b.events.len(), 1);
    assert_eq!(b.events[0].src, Source::Shell);
    assert_eq!(b.events[0].app.as_deref(), Some("hidden"));
}

#[test]
fn invalid_kind_refused() {
    let (h, _t) = test_hub(16, 100);
    assert_eq!(accept(&h, 0, "Media.x"), Err(Refusal::InvalidKind));
    assert_eq!(h.stats()["refused"], json!(1));
    assert!(h.read(0, &Filter::default(), None).events.is_empty());
}

#[test]
fn wait_gets_later_event_from_another_thread() {
    let (h, _t) = test_hub(16, 100);
    let h2 = h.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(50));
        accept(&h2, 0, "media.ended").unwrap();
    });
    let filter = Filter {
        kind: vec!["media.ended".to_string()],
        ..Default::default()
    };
    let w = h.wait(0, &filter, 5000);
    assert!(!w.timeout);
    let e = w.event.expect("the event");
    assert_eq!(e.kind, "media.ended");
    assert_eq!(w.next, e.seq + 1);
}

#[test]
fn wait_times_out() {
    let (h, t) = test_hub(16, 100);
    advance_after(t, 50, 500);
    let w = h.wait(0, &Filter::default(), 100);
    assert!(w.timeout);
    assert!(w.event.is_none());
}

#[test]
fn settle_true_after_present_and_settle_time() {
    let (h, t) = test_hub(16, 100);
    h.present(Some("player"), 1, &[[0, 0, 100, 50], [10, 10, 200, 80]], 1000);
    // not yet: no settle time has passed
    advance_after(t.clone(), 50, 500);
    let s = h.settle(0, 1000, 100);
    assert!(!s.settled);
    // now past the settle time
    t.store(3000, Ordering::Relaxed);
    let s = h.settle(0, 1000, 100);
    assert!(s.settled);
    assert_eq!(s.present_seq, 1);
    assert_eq!(s.app.as_deref(), Some("player"));
    assert_eq!(s.rect, Some([0, 0, 210, 90]));
}

#[test]
fn settle_wakes_when_the_screen_settles() {
    // a real clock: the condvar waits in real time
    let cfg = Config {
        cap: 16,
        max_read: 64,
        every_ms: 100,
        burst: 100,
        socket: "/tmp/lucina-test-unused.sock".into(),
        read_socket: "/tmp/lucina-test-unused-read.sock".into(),
        kernel: None,
    };
    let h = Hub::new(cfg, lucina_events::hub::monotonic_ms);
    h.present(Some("player"), 1, &[[0, 0, 10, 10]], 1000);
    let start = std::time::Instant::now();
    let s = h.settle(0, 100, 5000);
    assert!(s.settled);
    assert!(start.elapsed() < Duration::from_secs(1));
}

#[test]
fn flooded_event_is_not_a_gap() {
    let (h, t) = test_hub(16, 3);
    for i in 0..3 {
        assert_eq!(accept(&h, i, "media.step"), Ok(()));
    }
    // four events in the same ms: the fourth is flooded
    assert_eq!(accept(&h, 3, "media.step"), Err(Refusal::Flooded));
    // time passes and a fifth comes: one flood note, and no gap note
    t.store(2500, Ordering::Relaxed);
    assert_eq!(accept(&h, 4, "media.step"), Ok(()));
    let b = h.read(0, &Filter::default(), None);
    let lost: Vec<&lucina_events::wire::Event> = b
        .events
        .iter()
        .filter(|e| e.kind == "events.lost")
        .collect();
    assert_eq!(lost.len(), 1);
    assert_eq!(lost[0].fields["reason"], json!("flood"));
    assert_eq!(lost[0].fields["count"], json!(1));
    assert!(
        b.events
            .iter()
            .all(|e| !(e.kind == "events.lost" && e.fields["reason"] == json!("gap")))
    );
}

#[test]
fn shell_event_carries_its_app_field() {
    let (h, _t) = test_hub(16, 100);
    h.accept(
        Source::Shell,
        None,
        rules::INTROSPECT_ALL,
        (Source::Shell, 2),
        0,
        "app.front",
        Some("video"),
        vec![f("app", json!("video"), rules::VIS_PUBLIC)],
    )
    .unwrap();
    let filter = Filter {
        app: Some("video".to_string()),
        ..Default::default()
    };
    let b = h.read(0, &filter, None);
    assert_eq!(b.events.len(), 1);
    assert_eq!(b.events[0].app.as_deref(), Some("video"));
    // a filter for another app does not take it
    let other = Filter {
        app: Some("player".to_string()),
        ..Default::default()
    };
    assert!(h.read(0, &other, None).events.is_empty());
}

fn shell(h: &Hub, src_seq: u64, kind: &str, subject: Option<&str>, app_field: Option<&str>) {
    let fields = match app_field {
        Some(a) => vec![f("app", json!(a), rules::VIS_PUBLIC)],
        None => vec![],
    };
    h.accept(
        Source::Shell,
        None,
        rules::INTROSPECT_ALL,
        (Source::Shell, 2),
        src_seq,
        kind,
        subject,
        fields,
    )
    .unwrap();
}

#[test]
fn state_app_focused_keeps_latest() {
    let (h, _t) = test_hub(16, 100);
    shell(&h, 0, "app.focused", Some("player"), None);
    shell(&h, 1, "app.focused", Some("video"), None);
    let s = h.state();
    assert!(s.ok);
    assert_eq!(s.events.len(), 1);
    assert_eq!(s.events[0].kind, "app.focused");
    assert_eq!(s.events[0].subject.as_deref(), Some("video"));
}

#[test]
fn state_power_and_net() {
    let (h, _t) = test_hub(16, 100);
    shell(&h, 0, "power.battery", Some("bat0"), None);
    shell(&h, 1, "power.battery", Some("bat0"), None);
    shell(&h, 2, "power.charger", None, None);
    shell(&h, 3, "net.up", Some("eth0"), None);
    shell(&h, 4, "net.down", Some("eth0"), None);
    shell(&h, 5, "net.up", Some("wlan0"), None);
    let s = h.state();
    let kinds: Vec<&str> = s.events.iter().map(|e| e.kind.as_str()).collect();
    assert_eq!(kinds, vec!["power.battery", "power.charger", "net.down", "net.up"]);
    // sorted by seq, one slot per subject for net
    assert_eq!(s.events[2].subject.as_deref(), Some("eth0"));
    assert_eq!(s.events[3].subject.as_deref(), Some("wlan0"));
    assert!(s.events.iter().all(|e| e.seq < 6));
}

#[test]
fn state_media_started_per_app_removed_by_end() {
    let (h, _t) = test_hub(16, 100);
    shell(&h, 0, "media.started", Some("t1"), Some("player"));
    shell(&h, 1, "media.started", Some("t2"), Some("video"));
    shell(&h, 2, "media.ended", Some("t1"), Some("player"));
    let s = h.state();
    assert_eq!(s.events.len(), 1);
    assert_eq!(s.events[0].kind, "media.started");
    assert_eq!(s.events[0].app.as_deref(), Some("video"));
    // media.stopped removes too
    shell(&h, 3, "media.stopped", Some("t2"), Some("video"));
    assert!(h.state().events.is_empty());
}

#[test]
fn state_storage_mounted_removed_by_unmounted() {
    let (h, _t) = test_hub(16, 100);
    shell(&h, 0, "storage.mounted", Some("/mnt/usb"), None);
    shell(&h, 1, "storage.mounted", Some("/mnt/sd"), None);
    shell(&h, 2, "storage.unmounted", Some("/mnt/usb"), None);
    let s = h.state();
    assert_eq!(s.events.len(), 1);
    assert_eq!(s.events[0].subject.as_deref(), Some("/mnt/sd"));
}

#[test]
fn state_survives_queue_drops() {
    let (h, _t) = test_hub(2, 100);
    shell(&h, 0, "app.focused", Some("player"), None);
    // overflow the queue of cap 2
    accept(&h, 0, "media.step").unwrap();
    accept(&h, 1, "media.step").unwrap();
    accept(&h, 2, "media.step").unwrap();
    let s = h.state();
    assert_eq!(s.events.len(), 1);
    assert_eq!(s.events[0].kind, "app.focused");
}

#[test]
fn read_filter_moves_cursor_past_unmatched() {
    let (h, _t) = test_hub(16, 100);
    accept(&h, 0, "media.started").unwrap();
    accept(&h, 1, "media.ended").unwrap();
    let filter = Filter {
        kind: vec!["media.ended".to_string()],
        ..Default::default()
    };
    let b = h.read(0, &filter, None);
    assert_eq!(b.events.len(), 1);
    assert_eq!(b.events[0].kind, "media.ended");
    assert_eq!(b.next, 2);
}
