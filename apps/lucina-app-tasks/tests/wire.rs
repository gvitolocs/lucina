//! The bytes on the wire: `lucina-account`'s client against a small HTTP server that answers as
//! secretary-server's tasks.rs does, with the real engine behind it. What the unit tests with the
//! in-process fake can't show: the query string, the JSON bodies, and that the answer parses.

#[path = "../src/account.rs"]
#[allow(dead_code)]
mod account;
#[path = "../src/cache.rs"]
#[allow(dead_code)]
mod cache;
#[path = "../src/sample.rs"]
mod sample;
#[path = "../src/server.rs"]
#[allow(dead_code)]
mod server;
#[allow(dead_code)]
#[path = "../src/store.rs"]
mod store;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use chrono::TimeZone;
use chrono_tz::Tz;
use lucina_account::{Client, Token};
use secretary_tasks::{Command, Engine};
use serde_json::{Value, json};

use account::Account;
use server::{Server, ServerError};
use store::query;

const TZ: Tz = chrono_tz::Europe::Copenhagen;
const TOKEN: &str = "sec_abcdefghijklmnopqrstuvwxyz0123456789_-ABCDE";

fn now() -> u64 {
    TZ.with_ymd_and_hms(2026, 11, 2, 12, 0, 0)
        .unwrap()
        .timestamp_millis() as u64
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
            out.push(if b[i] == b'+' { b' ' } else { b[i] });
            i += 1;
        }
    }
    String::from_utf8(out).unwrap()
}

/// `status_for` in secretary-server.
fn status(ok: bool, code: Option<&str>) -> u16 {
    match (ok, code) {
        (true, _) => 200,
        (_, Some("conflict")) => 409,
        (_, Some("storage_error")) => 500,
        _ => 422,
    }
}

/// Answers one request per connection until the test ends. `script` overrides an answer.
fn serve(
    engine: Arc<Mutex<Engine>>,
    seen: Arc<Mutex<Vec<String>>>,
    force: Option<(u16, &'static str)>,
) -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut line = String::new();
            r.read_line(&mut line).unwrap();
            let mut len = 0;
            let mut auth = String::new();
            loop {
                let mut h = String::new();
                r.read_line(&mut h).unwrap();
                if h.trim().is_empty() {
                    break;
                }
                let lower = h.to_ascii_lowercase();
                if let Some(v) = lower.strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap();
                }
                if lower.starts_with("authorization:") {
                    auth = h["authorization:".len()..].trim().to_owned();
                }
            }
            let mut body = vec![0; len];
            r.read_exact(&mut body).unwrap();
            let body = String::from_utf8(body).unwrap();
            let target = line.split_whitespace().nth(1).unwrap().to_owned();
            seen.lock().unwrap().push(format!(
                "{} {target}",
                line.split_whitespace().next().unwrap()
            ));
            assert_eq!(auth, format!("Bearer {TOKEN}"));
            let (code, answer) = if let Some((c, b)) = force {
                (c, b.to_owned())
            } else if let Some(q) = target.strip_prefix("/v1/tasks/state?") {
                let mut view = serde_json::Map::new();
                let mut tz = "UTC".to_owned();
                for pair in q.split('&') {
                    let (k, v) = pair.split_once('=').unwrap();
                    let v = decode(v);
                    match k {
                        "timezone" => tz = v,
                        "view" => {
                            view.insert("kind".into(), json!(v));
                        }
                        "days" => {
                            view.insert("days".into(), json!(v.parse::<u32>().unwrap()));
                        }
                        k => {
                            view.insert(k.into(), json!(v));
                        }
                    }
                }
                let req = json!({"timezone": tz, "command": {"type": "snapshot", "view": view}})
                    .to_string();
                let a = engine.lock().unwrap().handle_at(&req, now());
                (
                    status(a.ok, a.code.as_deref()),
                    serde_json::to_string(&a).unwrap(),
                )
            } else if target == "/v1/tasks/command" {
                let a = engine.lock().unwrap().handle_at(&body, now());
                (
                    status(a.ok, a.code.as_deref()),
                    serde_json::to_string(&a).unwrap(),
                )
            } else if target == "/v1/tasks/sync" {
                (
                    409,
                    json!({"error": {"code": "todoist_not_connected", "message": "Todoist isn't connected"}}).to_string(),
                )
            } else {
                (404, "{}".to_owned())
            };
            let _ = write!(
                s,
                "HTTP/1.1 {code} X\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{answer}",
                answer.len()
            );
        }
    });
    url
}

fn rig(
    force: Option<(u16, &'static str)>,
) -> (Account, Arc<Mutex<Engine>>, Arc<Mutex<Vec<String>>>) {
    let engine = Arc::new(Mutex::new(sample::engine(TZ, now())));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let url = serve(engine.clone(), seen.clone(), force);
    let client = Client::new(&url, Some(Token::parse(TOKEN).unwrap())).unwrap();
    (Account { client }, engine, seen)
}

#[test]
fn a_list_comes_back_through_the_query_string() {
    let (mut a, _, seen) = rig(None);
    let v = a.state(&query("today", None), TZ.name()).unwrap();
    assert_eq!(v.counts.overdue, 2);
    assert!(
        v.list
            .groups
            .iter()
            .flat_map(|g| &g.tasks)
            .any(|t| t.content == "Oat milk and bread")
    );
    assert_eq!(
        seen.lock().unwrap()[0],
        "GET /v1/tasks/state?timezone=Europe%2FCopenhagen&view=today"
    );
    // A search, with characters that need encoding, and a list with nested sub-tasks.
    let mut q = query("search", None);
    q.query = Some("milk & bread".into());
    a.state(&q, TZ.name()).unwrap();
    assert!(seen.lock().unwrap()[1].ends_with("&view=search&query=milk%20%26%20bread"));
    let v = a.state(&query("project", Some("p4")), TZ.name()).unwrap();
    let parent = v
        .list
        .groups
        .iter()
        .flat_map(|g| &g.tasks)
        .find(|t| t.content.starts_with("Exercises"))
        .unwrap();
    assert_eq!(parent.children.len(), 2, "children come through");
}

#[test]
fn a_command_changes_the_engine_and_a_repeat_changes_nothing() {
    let (mut a, engine, _) = rig(None);
    let c = Command::Complete {
        task_id: "t8".into(),
    };
    let q = query("today", None);
    let r = a.command("op-1", TZ.name(), &c, &q).unwrap();
    assert!(!r.replayed);
    let v = r.view.unwrap();
    assert!(
        !v.list
            .groups
            .iter()
            .flat_map(|g| &g.tasks)
            .any(|t| t.id == "t8")
    );
    let again = a.command("op-1", TZ.name(), &c, &q).unwrap();
    assert!(again.replayed);
    assert_eq!(engine.lock().unwrap().state().completed.len(), 1);
    // Quick add through the same route, and its parse.
    let c = Command::QuickAdd {
        text: "Call Marco tomorrow 9am #Work p1".into(),
        lang: None,
        project_id: None,
        section_id: None,
        default_due: None,
    };
    let r = a.command("op-2", TZ.name(), &c, &q).unwrap();
    assert!(r.task_id.is_some());
    assert_eq!(r.parsed.unwrap().content, "Call Marco");
}

#[test]
fn a_refused_command_is_a_refusal() {
    let (mut a, _, _) = rig(None);
    let c = Command::Complete {
        task_id: "no-such".into(),
    };
    let e = a
        .command("op-1", TZ.name(), &c, &query("today", None))
        .unwrap_err();
    assert!(
        matches!(e, ServerError::Refused { status: 422, .. }),
        "{e:?}"
    );
    assert!(matches!(
        a.sync().unwrap_err(),
        ServerError::Refused { status: 409, .. }
    ));
}

#[test]
fn gateway_errors_mean_offline_so_the_command_stays_queued() {
    let (mut a, _, _) = rig(Some((503, "{}")));
    let e = a.command(
        "op-1",
        TZ.name(),
        &Command::Complete {
            task_id: "t8".into(),
        },
        &query("today", None),
    );
    assert!(matches!(e, Err(ServerError::Offline(_))));
}

#[test]
fn no_server_at_all_is_offline_too() {
    let client = Client::new("http://127.0.0.1:9", Some(Token::parse(TOKEN).unwrap())).unwrap();
    let mut a = Account { client };
    assert!(matches!(
        a.state(&query("today", None), TZ.name()),
        Err(ServerError::Offline(_))
    ));
}

#[test]
fn a_dead_token_signs_out() {
    let dir = std::env::temp_dir().join(format!("tasks-wire-{}", std::process::id()));
    // SAFETY: only this test reads this variable in this binary.
    unsafe { std::env::set_var(lucina_account::credential::PATH_ENV, dir.join("credential")) };
    let (mut a, _, _) = rig(Some((
        401,
        r#"{"error":{"code":"unauthorized","message":"revoked"}}"#,
    )));
    assert!(matches!(
        a.state(&query("today", None), TZ.name()),
        Err(ServerError::SignedOut(_))
    ));
    let _ = std::fs::remove_dir_all(dir);
}

#[allow(dead_code)]
fn _unused(_: Value) {}

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
    let today = query("today", None);
    let v = a.state(&today, TZ.name()).unwrap();
    assert_eq!(v.timezone, TZ.name());
    let unique = format!("contract {}", std::process::id());
    let c = Command::QuickAdd {
        text: format!("{unique} today p1 @juno"),
        lang: None,
        project_id: None,
        section_id: None,
        default_due: None,
    };
    let op = format!("op-{}", std::process::id());
    let r = a.command(&op, TZ.name(), &c, &today).unwrap();
    let id = r.task_id.clone().unwrap();
    let v = r.view.unwrap();
    let t = v
        .list
        .groups
        .iter()
        .flat_map(|g| &g.tasks)
        .find(|t| t.id == id)
        .unwrap();
    assert_eq!(t.content, unique);
    assert_eq!(t.priority, 1);
    assert_eq!(t.labels, vec!["juno".to_string()]);
    assert!(a.command(&op, TZ.name(), &c, &today).unwrap().replayed);
    let r = a
        .command(
            &format!("{op}-2"),
            TZ.name(),
            &Command::Complete {
                task_id: id.clone(),
            },
            &today,
        )
        .unwrap();
    assert!(
        !r.view
            .unwrap()
            .list
            .groups
            .iter()
            .flat_map(|g| &g.tasks)
            .any(|t| t.id == id)
    );
    // A refusal, and what it looks like through lucina-account.
    let e = a
        .command(
            &format!("{op}-3"),
            TZ.name(),
            &Command::Complete {
                task_id: "no-such".into(),
            },
            &today,
        )
        .unwrap_err();
    assert!(matches!(e, ServerError::Refused { .. }), "{e:?}");
    eprintln!("refusal reads: {e:?}; sync: {:?}", a.sync());
}
