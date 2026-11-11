//! The client, sign-in and chat engine against the fake server: the contract's routes and status codes, end to end over HTTP.
use std::sync::Arc;
use std::time::{Duration, Instant};

use lucina_account::decisions::Outcome;
use lucina_account::engine::{Engine, SaveResult, StreamStatus};
use lucina_account::fake::{Fake, DEVICE_ID, TOKEN};
use lucina_account::secrets::{self, SaveSecret, Secret};
use lucina_account::signin::{Phase, SignIn, Step, MAX_CODES};
use lucina_account::timeline::{Delivery, TimelinePage};
use lucina_account::{client::TimelineQuery, credential, ApiError, Client, Token};
use serde_json::json;

fn token() -> Token {
    Token::parse(TOKEN).unwrap()
}

fn client(f: &Fake) -> Client {
    Client::new(f.base(), Some(token())).unwrap()
}

fn wait(what: &str, mut ok: impl FnMut() -> bool) {
    let start = Instant::now();
    while !ok() {
        assert!(start.elapsed() < Duration::from_secs(10), "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn engine(f: &Fake) -> Engine {
    Engine::start(client(f), None, Arc::new(|| {}))
}

fn texts(e: &Engine) -> Vec<String> {
    e.state().timeline.messages().map(|m| m.item.text.clone()).collect()
}

#[test]
fn sign_in_collects_the_token_once_and_keeps_it_out_of_errors_and_debug() {
    let f = Fake::start();
    let c = Client::new(f.base(), None).unwrap();
    assert!(!c.signed_in());
    let mut s = SignIn::new("Juno (Galaxy Book Go)");
    assert_eq!(s.phase(Instant::now()), Phase::Loading);
    let t0 = Instant::now();
    assert_eq!(s.tick(&c, t0), Step::Changed);
    let Phase::Ready { qr, short_code, seconds_left } = s.phase(t0) else { panic!() };
    assert!(qr.width >= 21);
    assert_eq!(short_code.as_deref().map(str::len), Some(6));
    assert!(seconds_left >= 119);
    assert_eq!(f.qr_device_names(), [Some("Juno (Galaxy Book Go)".to_owned())]);
    // Not due yet: no request.
    assert_eq!(s.tick(&c, t0 + Duration::from_millis(100)), Step::Idle);
    // Due, and still pending.
    assert_eq!(s.tick(&c, t0 + Duration::from_secs(2)), Step::Idle);
    f.approve_all();
    let Step::Token(got) = s.tick(&c, t0 + Duration::from_secs(4)) else { panic!("no token") };
    assert_eq!(s.phase(t0 + Duration::from_secs(4)), Phase::SignedIn);
    assert_eq!(got, token());
    assert!(!format!("{got:?}").contains("FAKE"));
    assert_eq!(s.tick(&c, t0 + Duration::from_secs(6)), Step::Idle);
    let signed = Client::new(f.base(), Some(got)).unwrap();
    let session = signed.session().unwrap();
    assert_eq!(session.device_id.as_deref(), Some(DEVICE_ID));
}

#[test]
fn a_denied_code_stops_and_a_button_starts_again() {
    let f = Fake::start();
    let c = Client::new(f.base(), None).unwrap();
    let mut s = SignIn::new("Juno");
    let t = Instant::now();
    s.tick(&c, t);
    f.deny_all();
    assert_eq!(s.tick(&c, t + Duration::from_secs(2)), Step::Changed);
    assert_eq!(s.phase(t), Phase::Denied);
    assert_eq!(s.tick(&c, t + Duration::from_secs(10)), Step::Idle);
    assert_eq!(f.qr_requests(), 1);
    s.again(t + Duration::from_secs(10));
    assert_eq!(s.tick(&c, t + Duration::from_secs(10)), Step::Changed);
    assert!(matches!(s.phase(t + Duration::from_secs(10)), Phase::Ready { .. }));
    assert_eq!(f.qr_requests(), 2);
}

#[test]
fn expired_codes_renew_up_to_the_limit_then_wait_for_the_button() {
    let f = Fake::start();
    f.set_qr_ttl(0);
    let c = Client::new(f.base(), None).unwrap();
    let mut s = SignIn::new("Juno");
    let mut t = Instant::now();
    for _ in 0..(MAX_CODES + 2) {
        s.tick(&c, t);
        t += Duration::from_secs(3);
    }
    assert_eq!(f.qr_requests(), MAX_CODES);
    assert_eq!(s.phase(t), Phase::Paused);
}

#[test]
fn an_unreachable_server_is_a_message_and_it_comes_back_by_itself() {
    // Nothing listens on this port.
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let c = Client::new(&format!("http://127.0.0.1:{port}"), None).unwrap();
    let mut s = SignIn::new("Juno");
    let t = Instant::now();
    assert_eq!(s.tick(&c, t), Step::Changed);
    let Phase::Unavailable { message, rate_limited } = s.phase(t) else { panic!() };
    assert!(!rate_limited);
    assert!(!message.contains('?'));
    assert_eq!(s.tick(&c, t + Duration::from_secs(1)), Step::Idle);
}

#[test]
fn a_revoked_token_answers_401() {
    let f = Fake::start();
    let c = client(&f);
    assert!(c.session().unwrap().authenticated);
    f.revoke();
    // `GET /v1/session` never answers 401; the others do.
    assert!(!c.session().unwrap().authenticated);
    assert!(matches!(c.approvals(), Err(ApiError::Unauthorized(_))));
    let e = engine(&f);
    wait("signed_out", || e.state().signed_out);
    let bad = Client::new(f.base(), None).unwrap();
    assert!(matches!(bad.approvals(), Err(ApiError::Unauthorized(_))));
}

#[test]
fn the_token_file_round_trips_for_the_apps() {
    let p = std::env::temp_dir().join(format!("lucina-account-it-{}/credential", std::process::id()));
    credential::save(&p, &token()).unwrap();
    assert_eq!(credential::load(&p).unwrap(), Some(token()));
    credential::remove(&p).unwrap();
}

#[test]
fn history_stream_and_send_merge_into_one_list() {
    let f = Fake::start();
    f.add_item("lucina", "other", "Buongiorno", json!({}));
    f.add_item("user", "ios", "Dal telefono", json!({"meta": {"device": "dev_iphone"}}));
    let e = engine(&f);
    wait("history", || e.state().loaded);
    // The iPhone's own conversation is not "mine", as on the web.
    assert_eq!(texts(&e), ["Buongiorno"]);
    wait("the stream", || e.state().status == StreamStatus::Live);
    f.add_item("lucina", "other", "Dallo stream", json!({}));
    wait("the live item", || texts(&e).len() == 2);
    assert!(e.send("  ciao\u{7} ", None));
    wait("the reply", || texts(&e).len() == 4);
    assert_eq!(texts(&e), ["Buongiorno", "Dallo stream", "ciao", "Hai detto: ciao"]);
    assert!(e.state().timeline.messages().all(|m| m.delivery == Delivery::Sent));
    let sent: serde_json::Value = serde_json::from_str(&f.bodies("/v1/chat")[0]).unwrap();
    assert_eq!(sent["sessionId"], format!("juno-{DEVICE_ID}"));
    assert_eq!(sent["message"], "ciao");
    // The stream's copies of both come too and change nothing.
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(texts(&e).len(), 4);
}

#[test]
fn a_failed_send_stays_visible_and_a_retry_keeps_its_client_message_id() {
    let f = Fake::start();
    let e = engine(&f);
    wait("loaded", || e.state().loaded);
    f.fail_chats(1);
    assert!(e.send("riprova", None));
    wait("the failure", || e.state().timeline.messages().any(|m| matches!(m.delivery, Delivery::Failed(_))));
    let local = e.state().timeline.messages().next().unwrap().item.id.clone();
    assert!(e.retry(&local));
    wait("the reply", || texts(&e).len() == 2);
    let bodies = f.bodies("/v1/chat");
    assert_eq!(bodies.len(), 2);
    let cmid = |b: &str| serde_json::from_str::<serde_json::Value>(b).unwrap()["clientMessageId"].clone();
    assert_eq!(cmid(&bodies[0]), cmid(&bodies[1]));
}

#[test]
fn a_send_that_is_empty_too_long_or_a_secret_command_does_nothing() {
    let f = Fake::start();
    let e = engine(&f);
    assert!(!e.send("  \n ", None));
    assert!(!e.send(&"x".repeat(12_001), None));
    assert!(!e.send("/secret KEY value", None));
    assert!(e.state().timeline.is_empty());
    assert!(f.bodies("/v1/chat").is_empty());
}

#[test]
fn a_lost_connection_resumes_from_the_last_id_and_fills_the_gap() {
    let f = Fake::start();
    let e = engine(&f);
    wait("live", || e.state().status == StreamStatus::Live);
    let id = f.add_item("lucina", "other", "uno", json!({}));
    wait("uno", || texts(&e).len() == 1);
    f.end_streams();
    wait("a second stream", || f.streams_opened() >= 2);
    assert_eq!(f.last_event_ids().last().unwrap().as_deref(), Some(id.as_str()));
    f.add_item("lucina", "other", "due", json!({}));
    wait("due", || texts(&e).len() == 2);
}

#[test]
fn older_pages_and_an_expired_cursor() {
    let f = Fake::start();
    for i in 0..120 {
        f.add_item("lucina", "other", &format!("m{i}"), json!({}));
    }
    let e = engine(&f);
    wait("first page", || e.state().loaded);
    assert_eq!(texts(&e).len(), 50);
    assert!(e.state().timeline.has_older());
    e.load_older();
    wait("second page", || texts(&e).len() == 100);
    e.load_older();
    wait("third page", || texts(&e).len() == 120);
    assert!(!e.state().timeline.has_older());
    assert_eq!(texts(&e)[0], "m0");
    let sorted: Vec<i64> = e.state().timeline.messages().map(|m| m.item.ts_ms).collect();
    assert!(sorted.windows(2).all(|w| w[0] < w[1]));

    let cursor = e.state().timeline.cursor.clone().unwrap();
    f.expire_cursor(&cursor);
    e.resync();
    wait("the fresh window", || texts(&e).len() == 50);
}

#[test]
fn a_conversation_being_read_opens_no_stream_and_cannot_send() {
    let f = Fake::start();
    f.add_item("user", "ios", "Dal telefono", json!({"meta": {"device": "dev_iphone"}}));
    f.add_item("lucina", "other", "Altro", json!({}));
    let e = Engine::start(client(&f), Some("device:dev_iphone".into()), Arc::new(|| {}));
    wait("the page", || e.state().loaded);
    assert_eq!(texts(&e), ["Dal telefono"]);
    assert!(e.read_only());
    assert!(!e.send("no", None));
    assert_eq!(f.streams_opened(), 0);
    let list = client(&f).conversations().unwrap();
    assert!(list.iter().any(|c| c.title.contains("iPhone")));
}

#[test]
fn a_secret_goes_only_through_its_route() {
    let f = Fake::start();
    let value = format!("sk-{}", "Zq7".repeat(12));
    f.add_secret_request(json!({"id": "r1", "name": "MESHY_API_KEY", "project": "lucina", "environment": "prod", "path": "/", "reason": "x", "who": "Claude", "status": "active"}));
    f.set_projects(json!({"projects": [{"id": "p1", "name": "Lucina", "alias": "lucina", "environments": [{"slug": "prod"}]}]}));
    let e = engine(&f);
    wait("the queue", || e.state().secret_queue.is_some());
    e.load_catalog();
    wait("the catalog", || e.state().catalog.is_some());

    // The guard catches a paste in the composer before there is a bubble.
    let typed = format!("ecco la chiave {value}");
    let spans = secrets::detect(&typed);
    assert!(!spans.is_empty());
    assert_eq!(secrets::mask(&typed, &spans), "ecco la chiave [secret:token]");

    let project = {
        let st = e.state();
        secrets::find_project("lucina", st.catalog.as_ref().unwrap()).unwrap().id.clone()
    };
    f.add_existing_secret("MESHY_API_KEY");
    e.save_secret(SaveSecret {
        name: "MESHY_API_KEY".into(),
        project,
        environment: "prod".into(),
        path: "/".into(),
        overwrite: false,
        value: Secret::new(format!("{value}\n")),
        request: Some("r1".into()),
    });
    wait("exists", || e.state().save_result.is_some());
    let r = e.state().save_result.take().unwrap();
    // One more tap replaces it, with the value it still holds.
    let SaveResult::Exists(mut again) = r.1 else { panic!("not exists") };
    again.overwrite = true;
    e.save_secret(again);
    wait("saved", || e.state().save_result.is_some());
    assert!(matches!(e.state().save_result.take().unwrap().1, SaveResult::Saved(_)));
    wait("the card turns saved", || e.state().secret_queue.as_ref().unwrap().find("r1").unwrap().status == "saved");

    let all = f.all_bodies();
    let with_value: Vec<&str> = all.iter().filter(|(_, b)| b.contains(&value)).map(|(p, _)| p.as_str()).collect();
    assert_eq!(with_value, ["/v1/secrets", "/v1/secrets"]);
    assert!(f.bodies("/v1/chat").is_empty());
    let sent: serde_json::Value = serde_json::from_str(&f.bodies("/v1/secrets")[1]).unwrap();
    assert_eq!(sent["value"], value.as_str());
}

#[test]
fn decisions_and_approvals_over_the_wire() {
    let f = Fake::start();
    f.fill_demo();
    let c = client(&f);
    let pending = c.decisions(Some("pending"), 50).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].options.len(), 3);
    assert_eq!(c.decision("dc-demo0").unwrap().chosen_option.as_deref(), Some("Sì"));
    assert_eq!(c.decision("nope").unwrap_err().status(), Some(404));

    let list = c.approvals().unwrap();
    assert_eq!(list.len(), 2);
    let plain = list.iter().find(|a| !a.sensitive).unwrap();
    let sensitive = list.iter().find(|a| a.sensitive).unwrap();
    assert!(plain.can_approve() && !sensitive.can_approve());

    // The server refuses a sensitive approve on its own too.
    assert_eq!(Outcome::of(true, c.approval_decide(&sensitive.id, true, &sensitive.payload_hash)), Outcome::NeedsFaceId);
    assert_eq!(Outcome::of(false, c.approval_decide(&sensitive.id, false, &sensitive.payload_hash)), Outcome::Denied);
    assert_eq!(Outcome::of(true, c.approval_decide(&plain.id, true, "another hash")), Outcome::Changed);
    assert_eq!(Outcome::of(true, c.approval_decide(&plain.id, true, &plain.payload_hash)), Outcome::Approved);
    assert_eq!(Outcome::of(true, c.approval_decide(&plain.id, true, &plain.payload_hash)), Outcome::Decided);
    assert_eq!(Outcome::of(true, c.approval_decide("gone", true, "x")), Outcome::Gone);
    assert_eq!(c.approval(&sensitive.id).unwrap().status, "denied");
}

#[test]
fn a_poll_token_never_reaches_an_error() {
    // Nothing answers here: the error is about the connection and holds no query.
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let c = Client::new(&format!("http://127.0.0.1:{port}"), None).unwrap();
    let e = c.qr_poll("verysecretpolltoken").unwrap_err();
    assert!(!e.to_string().contains("verysecretpolltoken"));
    let _ = TimelinePage::default();
    let _ = TimelineQuery::default();
}

#[test]
fn any_stream_comes_as_blocks_and_resumes_after_an_id() {
    let f = Fake::start();
    let c = client(&f);
    let id = f.add_item("lucina", "other", "first", json!({}));
    let mut s = c.sse("/v1/timeline/stream", Some("tl_000000")).unwrap();
    let b = s.next_block().unwrap().expect("a block");
    assert_eq!(b.event, "item");
    assert_eq!(b.id.as_deref(), Some(id.as_str()));
    assert!(b.data.contains("first"));
    f.end_streams();
    assert_eq!(f.last_event_ids().last().cloned().flatten().as_deref(), Some("tl_000000"));
    f.revoke();
    assert!(matches!(c.sse("/v1/timeline/stream", None).map(|_| ()), Err(ApiError::Unauthorized(_))));
}
