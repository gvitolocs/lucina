use std::sync::Arc;
use std::thread;
use std::time::Duration;

use lucina_events::hub::{self, Config, Hub};
use lucina_events::wire::Filter;
use lucina_events::{emit, Reader};

#[test]
fn socket_flow() {
    let dir = std::env::temp_dir().join(format!("lucina-events-sock-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let sock = dir.join("events.sock");
    let rsock = dir.join("events-read.sock");
    let cfg = Config {
        socket: sock.clone(),
        read_socket: rsock.clone(),
        kernel: None,
        ..Default::default()
    };
    let hub = Arc::new(Hub::new(cfg.clone(), hub::monotonic_ms));
    let h = hub.clone();
    thread::spawn(move || {
        let _ = hub::serve(h, &cfg);
    });
    for _ in 0..200 {
        if sock.exists() && rsock.exists() {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(sock.exists() && rsock.exists());

    unsafe {
        std::env::set_var("LUCINA_EVENTS_SOCKET", &sock);
        std::env::set_var("LUCINA_APP_ID", "player");
    }
    emit("media.ended")
        .subject("track1")
        .field("codec", "opus")
        .send();

    let filter = Filter {
        kind: vec!["media.ended".to_string()],
        ..Default::default()
    };

    if unsafe { libc::geteuid() } == 0 {
        let mut r = Reader::connect(&rsock).expect("read socket");
        let w = r.wait(0, &filter, 3000).expect("wait answer");
        let e = w.event.expect("the emitted event");
        assert_eq!(e.kind, "media.ended");
        assert_eq!(e.app.as_deref(), Some("player"));
        assert_eq!(e.subject.as_deref(), Some("track1"));
        assert_eq!(e.fields["codec"], serde_json::json!("opus"));
        let stats = r.stats().expect("stats");
        assert_eq!(stats["ok"], serde_json::json!(true));
    } else {
        // the read socket is root-only: the hub refuses us
        let mut r = Reader::connect(&rsock).expect("read socket file");
        assert!(r.wait(0, &filter, 500).is_err());
        // the wait path through the Hub directly still sees the event
        let w = hub.wait(0, &filter, 1000);
        assert!(!w.timeout);
        assert_eq!(w.event.expect("event").app.as_deref(), Some("player"));
    }

    let _ = std::fs::remove_dir_all(&dir);
}
