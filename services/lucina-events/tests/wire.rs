use lucina_events::wire::{
    self, union, Ask, Batch, Event, Failure, Field, Filter, In, Settled, Snapshot, Source, Waited,
};
use serde_json::{json, Map, Value};

#[test]
fn rect_union_is_xywh() {
    assert_eq!(union([0, 0, 10, 10], [20, 5, 10, 10]), [0, 0, 30, 15]);
    // an empty rect (w or h <= 0) is ignored
    assert_eq!(union([0, 0, 0, 5], [20, 5, 10, 10]), [20, 5, 10, 10]);
    assert_eq!(union([0, 0, 10, 10], [5, 5, 0, 0]), [0, 0, 10, 10]);
    assert_eq!(union([0, 0, -3, 4], [0, 0, 0, 0]), [0, 0, 0, 0]);
    assert_eq!(union([10, 10, 5, 5], [0, 0, 4, 4]), [0, 0, 15, 15]);
}

fn event() -> Event {
    let mut fields = Map::new();
    fields.insert("codec".to_string(), json!("opus"));
    Event {
        seq: 7,
        src: Source::App,
        src_seq: 3,
        t_ms: 42,
        kind: "media.ended".to_string(),
        app: Some("player".to_string()),
        subject: Some("track1".to_string()),
        fields,
    }
}

#[test]
fn in_round_trip() {
    let lines = vec![
        In::Hello {
            src: Source::App,
            app: Some("player".to_string()),
            introspect: Some("all".to_string()),
        },
        In::Hello {
            src: Source::Shell,
            app: None,
            introspect: None,
        },
        In::Emit {
            src_seq: 3,
            kind: "media.ended".to_string(),
            subject: Some("track1".to_string()),
            fields: vec![
                Field {
                    n: "codec".to_string(),
                    v: json!("opus"),
                    vis: 0,
                },
                Field {
                    n: "note".to_string(),
                    v: json!("secret"),
                    vis: 2,
                },
            ],
        },
        In::Present {
            frame: 99,
            rects: vec![[0, 0, 100, 50], [10, 10, 20, 20]],
            gpu: Some(1),
        },
        In::Present {
            frame: 100,
            rects: vec![],
            gpu: None,
        },
    ];
    for line in &lines {
        let s = serde_json::to_string(line).unwrap();
        let back: In = serde_json::from_str(&s).unwrap();
        assert_eq!(*line, back);
    }
    let hello = serde_json::to_value(&lines[0]).unwrap();
    assert_eq!(hello["op"], "hello");
    assert_eq!(hello["src"], "app");
    let emit = serde_json::to_value(&lines[2]).unwrap();
    assert_eq!(emit["op"], "emit");
    assert_eq!(emit["fields"][1]["vis"], 2);
}

#[test]
fn ask_round_trip() {
    let asks = vec![
        Ask::Read {
            since: 5,
            filter: Filter::default(),
            max: Some(10),
        },
        Ask::Read {
            since: 5,
            filter: Filter {
                kind: vec!["media.*".to_string()],
                ..Default::default()
            },
            max: None,
        },
        Ask::Wait {
            since: 0,
            filter: Filter::default(),
            timeout_ms: 500,
        },
        Ask::Settle {
            since: 2,
            settle_ms: 120,
            timeout_ms: 1000,
        },
        Ask::Stats {},
        Ask::State {},
    ];
    for a in asks {
        let s = serde_json::to_string(&a).unwrap();
        let back: Ask = serde_json::from_str(&s).unwrap();
        assert_eq!(a, back);
    }
    // filter defaults to empty when missing
    let a: Ask = serde_json::from_str(r#"{"op":"read","since":1,"max":3}"#).unwrap();
    assert_eq!(
        a,
        Ask::Read {
            since: 1,
            filter: Filter::default(),
            max: Some(3)
        }
    );
    let s = serde_json::to_value(&Ask::Stats {}).unwrap();
    assert_eq!(s, json!({"op": "stats"}));
    let s = serde_json::to_value(&Ask::State {}).unwrap();
    assert_eq!(s, json!({"op": "state"}));
}

#[test]
fn answers_round_trip() {
    let batch = Batch {
        ok: true,
        events: vec![event()],
        next: 8,
        lost: 2,
        reset: false,
    };
    let s = serde_json::to_string(&batch).unwrap();
    assert_eq!(serde_json::from_str::<Batch>(&s).unwrap(), batch);

    let waited = Waited {
        ok: true,
        event: Some(event()),
        timeout: false,
        next: 8,
        lost: 0,
    };
    let s = serde_json::to_string(&waited).unwrap();
    assert_eq!(serde_json::from_str::<Waited>(&s).unwrap(), waited);

    let waited_timeout = Waited {
        ok: true,
        event: None,
        timeout: true,
        next: 8,
        lost: 1,
    };
    let s = serde_json::to_string(&waited_timeout).unwrap();
    assert_eq!(
        serde_json::from_str::<Waited>(&s).unwrap(),
        waited_timeout
    );

    let settled = Settled {
        ok: true,
        settled: true,
        present_seq: 4,
        app: Some("player".to_string()),
        rect: Some([0, 0, 200, 100]),
    };
    let s = serde_json::to_string(&settled).unwrap();
    assert_eq!(serde_json::from_str::<Settled>(&s).unwrap(), settled);

    let snap = Snapshot {
        ok: true,
        events: vec![event()],
    };
    let s = serde_json::to_string(&snap).unwrap();
    assert_eq!(serde_json::from_str::<Snapshot>(&s).unwrap(), snap);

    let fail = Failure {
        ok: false,
        error: "bad kind pattern: *".to_string(),
    };
    let s = serde_json::to_string(&fail).unwrap();
    assert_eq!(s, r#"{"ok":false,"error":"bad kind pattern: *"}"#);
    assert_eq!(serde_json::from_str::<Failure>(&s).unwrap(), fail);
}

#[test]
fn event_serde_skips() {
    let mut e = event();
    e.app = None;
    e.subject = None;
    e.fields = Map::new();
    let s = serde_json::to_string(&e).unwrap();
    let v = serde_json::from_str::<serde_json::Value>(&s).unwrap();
    assert!(v.get("app").is_none());
    assert!(v.get("subject").is_none());
    assert!(v.get("fields").is_none());
    assert_eq!(serde_json::from_str::<Event>(&s).unwrap(), e);
}

#[test]
fn kernel_line_parse() {
    let ke = wire::parse_kernel("3 1234 media.ended track%201 codec=opus").unwrap();
    assert_eq!(ke.src_seq, 3);
    assert_eq!(ke.t_ms, 1234);
    assert_eq!(ke.kind, "media.ended");
    assert_eq!(ke.subject.as_deref(), Some("track 1"));
    assert_eq!(ke.fields["codec"], Value::String("opus".to_string()));

    let ke = wire::parse_kernel("0 1 power.low - level=42 plugged=true").unwrap();
    assert_eq!(ke.subject, None);
    assert_eq!(ke.fields["level"], json!(42));
    assert_eq!(ke.fields["plugged"], json!(true));

    let ke = wire::parse_kernel("1 2 a.b s%3Dx%25y%0Az v=a%3Db").unwrap();
    assert_eq!(ke.subject.as_deref(), Some("s=x%y\nz"));
    assert_eq!(ke.fields["v"], Value::String("a=b".to_string()));

    let ke = wire::parse_kernel("1 2 a.b - neg=-7 flag=false").unwrap();
    assert_eq!(ke.fields["neg"], json!(-7));
    assert_eq!(ke.fields["flag"], json!(false));
}

#[test]
fn kernel_line_bad() {
    assert!(wire::parse_kernel("").is_none());
    assert!(wire::parse_kernel("x 1 a.b -").is_none());
    assert!(wire::parse_kernel("1 y a.b -").is_none());
    assert!(wire::parse_kernel("1 2 Bad.kind -").is_none());
    assert!(wire::parse_kernel("1 2 a.b - novalue").is_none());
    assert!(wire::parse_kernel("1 2 a.b - k=%zz").is_none());
    assert!(wire::parse_kernel("1 2 a.b - k=%2").is_none());
}

#[test]
fn kernel_encode_round_trip() {
    let fields = vec![
        ("level".to_string(), json!(42)),
        ("name".to_string(), json!("a b=c%d\ne")),
        ("on".to_string(), json!(true)),
    ];
    let line = wire::encode_kernel(5, 999, "power.low", Some("bat 0"), &fields);
    let ke = wire::parse_kernel(&line).unwrap();
    assert_eq!(ke.src_seq, 5);
    assert_eq!(ke.t_ms, 999);
    assert_eq!(ke.kind, "power.low");
    assert_eq!(ke.subject.as_deref(), Some("bat 0"));
    assert_eq!(ke.fields["level"], json!(42));
    assert_eq!(ke.fields["name"], Value::String("a b=c%d\ne".to_string()));
    assert_eq!(ke.fields["on"], json!(true));

    let line = wire::encode_kernel(0, 0, "k.x", None, &[]);
    let ke = wire::parse_kernel(&line).unwrap();
    assert_eq!(ke.subject, None);
    assert!(ke.fields.is_empty());
}
