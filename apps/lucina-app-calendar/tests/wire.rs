//! The bytes on the wire: `lucina-account`'s client against a small HTTP server that answers as
//! secretary-server's calendar.rs does (camelCase events from the engine, `confirm` required,
//! `read_only_source` for iPhone events). What the in-process fake can't show.

#[path = "../src/account.rs"]
#[allow(dead_code)]
mod account;
#[path = "../src/sample.rs"]
mod sample;
#[path = "../src/server.rs"]
#[allow(dead_code)]
mod server;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use chrono_tz::Tz;
use lucina_account::{Client, Token};
use serde_json::{Value, json};

use account::Account;
use server::{Draft, Fake, Server, ServerError};

const TZ: Tz = chrono_tz::Europe::Copenhagen;
const TOKEN: &str = "sec_abcdefghijklmnopqrstuvwxyz0123456789_-ABCDE";

fn today() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 11, 4).unwrap()
}

fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap());
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap()
}

fn err_json(e: ServerError) -> (u16, String) {
    match e {
        ServerError::Refused {
            status,
            code,
            message,
        } => (
            status,
            json!({"error": {"code": code, "message": message}}).to_string(),
        ),
        other => (
            500,
            json!({"error": {"code": "x", "message": other.text()}}).to_string(),
        ),
    }
}

fn answer(f: &mut Fake, method: &str, target: &str, body: &str) -> (u16, String) {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    if path == "/v1/calendar/events" && method == "GET" {
        let mut from = None;
        let mut to = None;
        for pair in query.split('&') {
            let (k, x) = pair.split_once('=').unwrap();
            let t = DateTime::parse_from_rfc3339(&decode(x))
                .unwrap()
                .with_timezone(&Utc);
            if k == "from" {
                from = Some(t);
            } else {
                to = Some(t);
            }
        }
        let events = f.events(from.unwrap(), to.unwrap()).unwrap();
        return (
            200,
            json!({"from": "", "to": "", "timeZone": TZ.name(), "events": events}).to_string(),
        );
    }
    if path == "/v1/calendar/events" && method == "POST" {
        assert!(
            v.get("target").is_none(),
            "no target: Lucina's own calendar"
        );
        let d: Draft = serde_json::from_value(v).unwrap();
        return match f.create(&d) {
            Ok(e) => (201, json!({"created": true, "event": e}).to_string()),
            Err(e) => err_json(e),
        };
    }
    let id = decode(path.strip_prefix("/v1/calendar/events/").unwrap());
    if method == "PATCH" {
        if v["confirm"] != json!(true) {
            return (
                428,
                json!({"error": {"code": "confirmation_required", "message": ""}}).to_string(),
            );
        }
        let d: Draft = serde_json::from_value(v).unwrap();
        return match f.update(&id, &d) {
            Ok(e) => (200, json!({"updated": true, "event": e}).to_string()),
            Err(e) => err_json(e),
        };
    }
    assert_eq!(method, "DELETE");
    assert_eq!(query, "confirm=true");
    match f.delete(&id) {
        Ok(()) => (200, json!({"deleted": true, "id": id}).to_string()),
        Err(e) => err_json(e),
    }
}

fn rig() -> Account {
    let fake = Arc::new(Mutex::new(sample::server(TZ, today())));
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut line = String::new();
            r.read_line(&mut line).unwrap();
            let mut len = 0;
            loop {
                let mut h = String::new();
                r.read_line(&mut h).unwrap();
                if h.trim().is_empty() {
                    break;
                }
                if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap();
                }
            }
            let mut body = vec![0; len];
            r.read_exact(&mut body).unwrap();
            let mut parts = line.split_whitespace();
            let (method, target) = (parts.next().unwrap(), parts.next().unwrap());
            let (code, text) = answer(
                &mut fake.lock().unwrap(),
                method,
                target,
                std::str::from_utf8(&body).unwrap(),
            );
            let _ = write!(
                s,
                "HTTP/1.1 {code} X\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{text}",
                text.len()
            );
        }
    });
    Account {
        client: Client::new(&url, Some(Token::parse(TOKEN).unwrap())).unwrap(),
    }
}

fn november() -> (DateTime<Utc>, DateTime<Utc>) {
    (
        TZ.with_ymd_and_hms(2026, 11, 1, 0, 0, 0)
            .unwrap()
            .with_timezone(&Utc),
        TZ.with_ymd_and_hms(2026, 12, 1, 0, 0, 0)
            .unwrap()
            .with_timezone(&Utc),
    )
}

#[test]
fn events_arrive_as_the_engine_made_them() {
    let mut a = rig();
    let (from, to) = november();
    let list = a.events(from, to).unwrap();
    let review = list.iter().find(|e| e.title == "Design review").unwrap();
    assert!(review.id.starts_with("iphone:"));
    assert!(!review.editable);
    assert_eq!(review.attendees.len(), 2);
    let stand = list.iter().filter(|e| e.title == "Team stand-up").count();
    assert!(stand > 8, "one per instance: {stand}");
}

#[test]
fn a_new_event_lands_in_lucinas_calendar_through_the_route() {
    let mut a = rig();
    let d = Draft {
        title: "Coffee".into(),
        start: "2026-11-05T10:00:00.000Z".into(),
        end: Some("2026-11-05T10:30:00.000Z".into()),
        location: Some("".into()),
        ..Draft::default()
    };
    let e = a.create(&d).unwrap().unwrap();
    assert!(e.id.starts_with("lucina:"));
    assert!(e.editable);
    assert_eq!(e.location, None);
    // Edit it, with the confirmation the route demands, and delete it.
    let mut d2 = d.clone();
    d2.title = "Long coffee".into();
    let e2 = a.update(&e.id, &d2).unwrap().unwrap();
    assert_eq!(e2.title, "Long coffee");
    a.delete(&e.id).unwrap();
    let (from, to) = november();
    assert!(
        !a.events(from, to)
            .unwrap()
            .iter()
            .any(|x| x.title == "Long coffee")
    );
}

#[test]
fn iphone_events_are_refused_by_the_route() {
    let mut a = rig();
    let (from, to) = november();
    let id = a
        .events(from, to)
        .unwrap()
        .iter()
        .find(|e| e.title == "Dentist")
        .unwrap()
        .id
        .clone();
    let d = Draft {
        title: "x".into(),
        start: "2026-11-05".into(),
        ..Draft::default()
    };
    let e = a.update(&id, &d).unwrap_err();
    assert!(
        matches!(e, ServerError::Refused { status: 422, .. }),
        "{e:?}"
    );
    let e = a.delete(&id).unwrap_err();
    assert!(
        matches!(e, ServerError::Refused { status: 422, .. }),
        "{e:?}"
    );
}

#[test]
fn no_server_at_all_is_offline() {
    let mut a = Account {
        client: Client::new("http://127.0.0.1:9", Some(Token::parse(TOKEN).unwrap())).unwrap(),
    };
    let (from, to) = november();
    assert!(matches!(a.events(from, to), Err(ServerError::Offline(_))));
}

/// Against a real `secretary-server` started on this machine with an empty data directory:
/// `CONTRACT_URL=http://127.0.0.1:8899 CONTRACT_TOKEN_FILE=… cargo test`. Never the owner's.
#[test]
fn against_a_throwaway_secretary_server() {
    // Runs only when pointed at such a server.
    let Ok(url) = std::env::var("CONTRACT_URL") else {
        return;
    };
    assert!(
        url.starts_with("http://127.0.0.1"),
        "only a server on this machine"
    );
    let token = std::fs::read_to_string(std::env::var("CONTRACT_TOKEN_FILE").unwrap()).unwrap();
    let client = Client::new(&url, Some(Token::parse(token.trim()).unwrap())).unwrap();
    let mut a = Account { client };
    let (from, to) = november();
    a.events(from, to).unwrap();
    let d = Draft {
        title: "Contract coffee".into(),
        start: "2026-11-05T10:00:00.000Z".into(),
        end: Some("2026-11-05T10:30:00.000Z".into()),
        location: Some("".into()),
        description: Some("".into()),
        ..Draft::default()
    };
    let e = a.create(&d).unwrap().expect("the event comes back");
    assert!(e.id.starts_with("lucina:"), "{}", e.id);
    assert!(e.editable);
    assert_eq!(e.source, secretary_calendar::Source::Lucina);
    assert_eq!(e.start_ms, 1_793_872_800_000);
    assert!(a.events(from, to).unwrap().iter().any(|x| x.id == e.id));
    let mut d2 = d.clone();
    d2.title = "Long coffee".into();
    d2.end = Some("2026-11-05T11:00:00.000Z".into());
    let e2 = a.update(&e.id, &d2).unwrap().expect("the event comes back");
    assert_eq!(e2.title, "Long coffee");
    assert_eq!(e2.end_ms, 1_793_876_400_000);
    a.delete(&e.id).unwrap();
    assert!(!a.events(from, to).unwrap().iter().any(|x| x.id == e.id));
    // All day: the exclusive end as the web app sends it.
    let all = Draft {
        title: "Contract day".into(),
        start: "2026-11-06".into(),
        end: Some("2026-11-07".into()),
        all_day: true,
        ..Draft::default()
    };
    let e = a.create(&all).unwrap().expect("the event comes back");
    assert!(e.all_day);
    assert_eq!(
        (e.start.as_str(), e.end.as_str()),
        ("2026-11-06", "2026-11-07")
    );
    a.delete(&e.id).unwrap();
    // What the owner's events are.
    let r = a.update("iphone:abc@123", &d).unwrap_err();
    assert!(
        matches!(r, ServerError::Refused { status: 422, .. }),
        "{r:?}"
    );
    let r = a.delete("iphone:abc@123").unwrap_err();
    assert!(
        matches!(r, ServerError::Refused { status: 422, .. }),
        "{r:?}"
    );
}
