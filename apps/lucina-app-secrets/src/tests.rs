//! The app against the fake server over real HTTP and against sample mode's server. Every value here is made up. The expectations
//! follow `web/src/components/SecretCard.test.tsx` and `web/src/views/Secrets.*.test.tsx`.

use std::sync::mpsc;

use lucina_account::fake::{Fake, TOKEN};
use lucina_account::secrets::{SaveSecret, Secret};
use lucina_account::{ApiError, Client, Token};
use lucina_app::{App, Harness, keys};
use serde_json::json;

use crate::app::{Entry, Phase, Place, Secrets};
use crate::server::{Account, Job, Msg, Sample, Server, run};

const CANARY: &str = "synthetic-canary-value-0123456789";

struct T {
    h: Harness<Secrets>,
    server: Box<dyn Server>,
    jobs: mpsc::Receiver<Job>,
}

impl T {
    fn with(server: Box<dyn Server>) -> T {
        let (tx, jobs) = mpsc::channel();
        let app = Secrets::new(Some(Box::new(move |j| {
            let _ = tx.send(j);
        })));
        let mut t = T { h: Harness::new(app, 1280, 800), server, jobs };
        t.h.app().start();
        t.settle();
        t
    }

    fn settle(&mut self) {
        for _ in 0..6 {
            while let Ok(job) = self.jobs.try_recv() {
                let m = run(self.server.as_mut(), job);
                self.h.core.message(m);
            }
            self.h.core.input(lucina_app::Input::Wheel(0));
            self.h.core.paint();
        }
    }

    fn app(&mut self) -> &mut Secrets {
        self.h.app()
    }

    fn enter(&mut self) {
        self.h.key(keys::KEY_ENTER);
        self.settle();
    }
}

fn fake() -> Fake {
    let f = Fake::start();
    f.set_projects(json!({"projects": [{"id": "p-os", "name": "LucinaOS", "slug": "lucinaos-x1", "alias": "lucinaos", "environments": [{"slug": "dev", "name": "Dev"}, {"slug": "prod", "name": "Prod"}]}]}));
    f
}

fn request(f: &Fake, id: &str, name: &str, status: &str) {
    f.add_secret_request(json!({"id": id, "name": name, "project": "lucinaos", "environment": "prod", "path": "/", "reason": "The laptop joins the Wi-Fi", "who": "claude-code", "status": status}));
}

fn over_http(f: &Fake) -> T {
    let client = Client::new(f.base(), Token::parse(TOKEN)).unwrap();
    T::with(Box::new(Account { client }))
}

/// Everything sent anywhere, in one string.
fn everything_sent(f: &Fake) -> Vec<(String, String)> {
    f.all_bodies()
}

#[test]
fn the_active_request_opens_its_card_with_the_cursor_in_the_value() {
    let f = fake();
    request(&f, "sr-1", "WIFI_PSK", "active");
    let mut t = over_http(&f);
    let c = t.app().card.as_ref().expect("a card");
    assert_eq!(c.name.text, "WIFI_PSK");
    assert!(c.value.password);
    t.h.type_str(CANARY);
    assert_eq!(t.app().card.as_ref().unwrap().value.text, CANARY);
}

#[test]
fn paste_and_ok_saves_once_and_the_value_only_goes_in_that_body() {
    let f = fake();
    request(&f, "sr-1", "WIFI_PSK", "active");
    let mut t = over_http(&f);
    t.h.type_str(CANARY);
    t.enter();
    assert!(t.app().card.is_none(), "saved: the card closes");
    assert_eq!(t.app().receipt.as_deref(), Some("WIFI_PSK saved. Fingerprint ab12cd34."));
    let saves = f.bodies("/v1/secrets");
    assert_eq!(saves.len(), 1);
    let b: serde_json::Value = serde_json::from_str(&saves[0]).unwrap();
    assert_eq!(b["name"], "WIFI_PSK");
    assert_eq!(b["project"], "p-os");
    assert_eq!(b["environment"], "prod");
    assert_eq!(b["path"], "/");
    assert_eq!(b["value"], CANARY);
    assert_eq!(b["confirmed"], true);
    assert_eq!(b["request"], "sr-1");
    for (path, body) in everything_sent(&f) {
        assert!(path == "/v1/secrets" || !body.contains(CANARY), "{path}");
    }
    assert!(!format!("{:?}", t.app().entries).contains(CANARY));
}

#[test]
fn the_screen_never_depends_on_what_the_value_says() {
    // Same length, different letters: the frame must be identical to the pixel.
    let shot = |value: &str| {
        let mut t = T::with(Box::new(Sample::new()));
        t.h.type_str(value);
        t.h.core.input(lucina_app::Input::Wheel(0));
        t.h.core.paint();
        t.h.ppm()
    };
    let a = shot("aaaaaaaaaaaaaaaaaaaaaaaa");
    let b = shot("Zq7Zq7Zq7Zq7Zq7Zq7Zq7Zq7");
    assert!(a == b, "a screenshot would tell two values apart");
}

#[test]
fn replacing_takes_one_more_press_and_the_value_goes_again_without_being_typed() {
    let f = fake();
    f.add_existing_secret("WIFI_PSK");
    request(&f, "sr-1", "WIFI_PSK", "active");
    let mut t = over_http(&f);
    t.h.type_str(CANARY);
    t.enter();
    {
        let c = t.app().card.as_ref().expect("still open");
        assert_eq!(c.phase, Phase::Replace);
        assert!(c.value.text.is_empty(), "the field holds nothing once it left");
    }
    assert_eq!(f.bodies("/v1/secrets").len(), 1);
    t.app().resend(true);
    t.settle();
    assert!(t.app().card.is_none());
    assert_eq!(t.app().receipt.as_deref(), Some("WIFI_PSK replaced. Fingerprint ab12cd34."));
    let bodies = f.bodies("/v1/secrets");
    assert_eq!(bodies.len(), 2);
    let second: serde_json::Value = serde_json::from_str(&bodies[1]).unwrap();
    assert_eq!(second["overwrite"], true);
    assert_eq!(second["value"], CANARY);
}

#[test]
fn keeping_the_old_one_sends_nothing_more() {
    let f = fake();
    f.add_existing_secret("WIFI_PSK");
    request(&f, "sr-1", "WIFI_PSK", "active");
    let mut t = over_http(&f);
    t.h.type_str(CANARY);
    t.enter();
    // A new value drops the one waiting.
    t.h.type_str("x");
    t.settle();
    let c = t.app().card.as_ref().unwrap();
    assert_eq!(c.phase, Phase::Idle);
    assert!(c.held.is_none());
    assert_eq!(f.bodies("/v1/secrets").len(), 1);
}

#[test]
fn not_now_dismisses_and_the_value_goes_nowhere() {
    let f = fake();
    request(&f, "sr-1", "WIFI_PSK", "active");
    let mut t = over_http(&f);
    t.h.type_str(CANARY);
    t.app().not_now();
    t.settle();
    assert!(t.app().card.is_none());
    assert_eq!(f.bodies("/v1/secrets").len(), 0);
    let all = everything_sent(&f);
    assert!(all.iter().any(|(p, _)| p == "/v1/secrets/requests/sr-1/dismiss"));
    assert!(!all.iter().any(|(_, b)| b.contains(CANARY)));
    assert!(format!("{:?}", t.app().entries).contains("sr-1"));
}

#[test]
fn a_named_project_the_catalog_lacks_is_never_saved_elsewhere() {
    let f = fake();
    f.add_secret_request(json!({"id": "sr-1", "name": "K", "project": "nowhere", "environment": "prod", "path": "/", "reason": "r", "who": "w", "status": "active"}));
    let mut t = over_http(&f);
    t.h.type_str(CANARY);
    assert!(!t.app().card_ready());
    t.enter();
    assert_eq!(f.bodies("/v1/secrets").len(), 0, "OK does nothing");
    assert!(t.app().card.is_some());
}

#[test]
fn a_card_for_an_environment_the_project_lacks_cannot_be_saved() {
    let f = fake();
    f.add_secret_request(json!({"id": "sr-1", "name": "K", "project": "lucinaos", "environment": "staging", "path": "/", "reason": "r", "who": "w", "status": "active"}));
    let mut t = over_http(&f);
    t.h.type_str(CANARY);
    assert!(!t.app().card_ready());
}

#[test]
fn the_next_request_opens_after_this_one() {
    let mut t = T::with(Box::new(Sample::new()));
    assert_eq!(t.app().card.as_ref().unwrap().name.text, "WIFI_PSK");
    t.h.type_str(CANARY);
    t.enter();
    assert_eq!(t.app().card.as_ref().expect("the next one").name.text, "MAP_TOKEN");
    assert_eq!(t.app().receipt.as_deref().unwrap().split(" Fingerprint").next(), Some("WIFI_PSK saved."));
}

#[test]
fn esc_hides_the_card_and_the_request_stays_open_with_the_server() {
    let mut t = T::with(Box::new(Sample::new()));
    t.h.type_str(CANARY);
    t.h.key(keys::KEY_ESC);
    t.settle();
    assert!(t.app().card.is_none());
    // Not reopened by the next read of the queue.
    t.app().start();
    t.settle();
    assert!(t.app().card.is_none());
    // Enter on its row opens it again, empty.
    t.app().open_request("sr-1");
    let c = t.app().card.as_ref().unwrap();
    assert!(c.value.text.is_empty());
    assert_eq!(c.name.text, "WIFI_PSK");
}

#[test]
fn browsing_lists_names_only_and_walks_folders_by_keyboard() {
    let mut t = T::with(Box::new(Sample::new()));
    t.h.key(keys::KEY_ESC);
    t.settle();
    t.app().pick_project("p-sample".into());
    t.app().pick_environment("prod".into());
    t.settle();
    let entries = t.app().entries.clone();
    assert!(entries.contains(&Entry::Folder("wifi".into())));
    assert!(entries.contains(&Entry::Secret("SAMPLE_API_KEY".into())));
    let at = entries.iter().position(|e| *e == Entry::Folder("wifi".into())).unwrap();
    for _ in 0..at {
        t.h.key(keys::KEY_DOWN);
    }
    t.enter();
    assert_eq!(t.app().path, "/wifi");
    assert!(t.app().entries.contains(&Entry::Secret("HOME_PSK".into())));
    t.h.key(keys::KEY_BACKSPACE);
    t.settle();
    assert_eq!(t.app().path, "/");
}

#[test]
fn add_secret_is_the_same_card_with_a_name_and_a_place() {
    let f = fake();
    let mut t = over_http(&f);
    t.h.key(keys::KEY_A);
    t.settle();
    {
        let c = t.app().card.as_ref().expect("a card");
        assert!(c.request.is_none());
        assert_eq!(c.project.as_deref(), Some("p-os"));
    }
    t.h.type_str("MY_KEY");
    assert!(!t.app().card_ready(), "no value yet");
    t.h.key(keys::KEY_TAB);
    t.h.type_str(CANARY);
    assert!(t.app().card_ready());
    t.enter();
    let saves = f.bodies("/v1/secrets");
    assert_eq!(saves.len(), 1);
    assert!(saves[0].contains("MY_KEY"));
    assert_eq!(t.app().receipt.as_deref(), Some("MY_KEY saved. Fingerprint ab12cd34."));
}

#[test]
fn a_new_project_takes_a_review_and_says_what_it_made() {
    let f = fake();
    let mut t = over_http(&f);
    t.app().open_new_project("New one");
    t.settle();
    t.app().new_project.as_mut().unwrap().confirm = true;
    t.app().message(Msg::Created { result: Ok(lucina_account::secrets::Project { id: "p-new".into(), name: "New one".into(), slug: None, alias: None, environments: vec!["dev".into()] }), for_card: false });
    t.settle();
    assert!(t.app().new_project.is_none());
    assert_eq!(t.app().project.as_deref(), Some("p-new"));
    assert_eq!(t.app().receipt.as_deref(), Some("Project New one created (dev)."));
    let bodies = {
        let c = Client::new(f.base(), Token::parse(TOKEN)).unwrap();
        let p = c.secret_create_project("Other").unwrap();
        assert_eq!(p.environments, ["dev", "prod"]);
        f.bodies("/v1/secrets/projects")
    };
    assert_eq!(bodies.len(), 1);
    assert!(bodies[0].contains("\"confirmed\": true"), "{}", bodies[0]);
}

/// A server that fails its first save, and counts what it was asked.
struct Flaky {
    fails: u32,
    /// The save is carried out and its answer lost.
    lose_answers: u32,
    tries: std::sync::Arc<std::sync::atomic::AtomicU32>,
    inner: Sample,
}

impl Server for Flaky {
    fn catalog(&mut self) -> Result<Vec<lucina_account::secrets::Project>, ApiError> {
        self.inner.catalog()
    }
    fn names(&mut self, a: &str, b: &str, c: &str) -> Result<lucina_account::secrets::Folder, ApiError> {
        self.inner.names(a, b, c)
    }
    fn requests(&mut self) -> Result<lucina_account::secrets::SecretQueue, ApiError> {
        self.inner.requests()
    }
    fn dismiss(&mut self, id: &str) -> Result<(), ApiError> {
        self.inner.dismiss(id)
    }
    fn save(&mut self, save: &SaveSecret) -> Result<lucina_account::secrets::SecretReceipt, ApiError> {
        self.tries.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.fails > 0 {
            self.fails -= 1;
            return Err(ApiError::Network("timed out".into()));
        }
        let r = self.inner.save(save);
        if self.lose_answers > 0 {
            self.lose_answers -= 1;
            return Err(ApiError::Network("timed out".into()));
        }
        r
    }
    fn create_project(&mut self, name: &str) -> Result<lucina_account::secrets::Project, ApiError> {
        self.inner.create_project(name)
    }
}

#[test]
fn a_lost_connection_keeps_the_value_for_try_again_and_never_in_the_field() {
    let tries = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let mut t = T::with(Box::new(Flaky { fails: 1, lose_answers: 0, tries: tries.clone(), inner: Sample::new() }));
    t.h.type_str(CANARY);
    t.enter();
    {
        let c = t.app().card.as_ref().unwrap();
        assert_eq!(c.phase, Phase::Retry);
        assert!(c.error.as_deref().unwrap().contains("timed out"));
        assert!(c.value.text.is_empty());
        assert!(!format!("{:?}", c.held).contains("canary"));
    }
    t.app().resend(false);
    t.settle();
    assert_eq!(tries.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert_eq!(t.app().receipt.as_deref().unwrap().split(" Fingerprint").next(), Some("WIFI_PSK saved."));
}

#[test]
fn a_lost_answer_asks_before_it_replaces_what_arrived() {
    let tries = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let mut inner = Sample::new();
    // A blank card, so that the first save does not advance the queue under the test.
    inner.dismiss("sr-1").unwrap();
    inner.dismiss("sr-2").unwrap();
    let mut t = T::with(Box::new(Flaky { fails: 0, lose_answers: 1, tries: tries.clone(), inner }));
    t.h.key(keys::KEY_A);
    t.settle();
    t.h.type_str("MY_KEY");
    t.h.key(keys::KEY_TAB);
    t.h.type_str(CANARY);
    t.enter();
    assert_eq!(t.app().card.as_ref().unwrap().phase, Phase::Retry);
    t.app().resend(false);
    t.settle();
    // It did arrive: the server says it exists, and nothing is replaced without one more press.
    assert_eq!(t.app().card.as_ref().unwrap().phase, Phase::Replace);
    assert_eq!(tries.load(std::sync::atomic::Ordering::SeqCst), 2);
}

#[test]
fn a_dead_token_goes_to_sign_in_and_wipes_the_card() {
    let f = fake();
    request(&f, "sr-1", "WIFI_PSK", "active");
    let client = Client::new(f.base(), Token::parse(TOKEN)).unwrap();
    let mut server: Box<dyn Server> = Box::new(Account { client });
    let v = server.requests().unwrap();
    assert_eq!(v.requests.len(), 1);
    f.revoke();
    let m = run(server.as_mut(), Job::Queue);
    assert!(matches!(m, Msg::SignedOut));
    let save = SaveSecret { name: "K".into(), project: "p".into(), environment: "e".into(), path: "/".into(), overwrite: false, value: Secret::new(CANARY.into()), request: None };
    assert!(matches!(run(server.as_mut(), Job::Save(Box::new(save))), Msg::SignedOut));
}

#[test]
fn nothing_that_prints_shows_the_value() {
    let save = SaveSecret { name: "K".into(), project: "p".into(), environment: "e".into(), path: "/".into(), overwrite: false, value: Secret::new(CANARY.into()), request: None };
    assert!(!format!("{save:?}").contains("canary"));
    let m = Msg::NotSaved { save: Box::new(save), exists: false, message: "x".into() };
    if let Msg::NotSaved { save, .. } = &m {
        assert!(!format!("{save:?}").contains("canary"));
    }
}

#[test]
fn the_toolkit_knows_a_password_field_has_the_keyboard() {
    let mut t = T::with(Box::new(Sample::new()));
    assert!(t.h.core.typing_secret(), "the card opened with the cursor in the value");
    t.h.key(keys::KEY_ESC);
    t.settle();
    assert!(!t.h.core.typing_secret());
}

#[test]
fn a_quit_wipes_the_field() {
    let mut t = T::with(Box::new(Sample::new()));
    t.h.type_str(CANARY);
    t.app().on_quit();
    assert!(t.app().card.as_ref().unwrap().value.text.is_empty());
}

#[test]
fn the_last_place_only_holds_names() {
    let mut last = None;
    let saved = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Place>::new()));
    let s2 = saved.clone();
    let (tx, jobs) = mpsc::channel();
    let app = Secrets::new(Some(Box::new(move |j| {
        let _ = tx.send(j);
    })))
    .with_place(last.take(), Box::new(move |p| s2.lock().unwrap().push(p.clone())));
    let mut t = T { h: Harness::new(app, 1280, 800), server: Box::new(Sample::new()), jobs };
    t.h.app().start();
    t.settle();
    t.h.type_str(CANARY);
    t.enter();
    assert_eq!(saved.lock().unwrap()[0], Place { project: "p-sample".into(), environment: "prod".into(), path: "/".into() });
}

#[test]
fn every_screen_paints_at_both_sizes() {
    for (w, h) in [(800, 600), (1920, 1080)] {
        let (tx, jobs) = mpsc::channel();
        let app = Secrets::new(Some(Box::new(move |j| {
            let _ = tx.send(j);
        })));
        let mut t = T { h: Harness::new(app, w, h), server: Box::new(Sample::new()), jobs };
        t.h.app().start();
        t.settle();
        t.h.key(keys::KEY_ESC);
        t.settle();
        t.h.key(keys::KEY_A);
        t.settle();
        t.h.key(keys::KEY_ESC);
        t.settle();
        t.h.key(keys::KEY_N);
        t.settle();
        let _ = t.h.ppm();
    }
}

#[test]
fn lucina_reads_no_secret_name_or_value_but_works_the_buttons() {
    let tree = |t: &mut T| serde_json::to_string(&t.h.tree().reply(1)).unwrap();
    let f = fake();
    request(&f, "sr-1", "WIFI_PSK", "active");
    let mut t = over_http(&f);
    t.h.type_str(CANARY);
    let s = tree(&mut t);
    for word in ["WIFI_PSK", CANARY, "LucinaOS", "Wi-Fi", "claude-code"] {
        assert!(!s.contains(word), "{word} in the tree: {s}");
    }
    assert!(t.h.tree().text().contains("Secrets"));

    let mut t = T::with(Box::new(Sample::new()));
    t.h.key(keys::KEY_ESC);
    t.settle();
    t.app().pick_project("p-sample".into());
    t.app().pick_environment("prod".into());
    t.settle();
    assert!(t.app().entries.contains(&Entry::Secret("SAMPLE_API_KEY".into())));
    let s = tree(&mut t);
    // The secrets listed, their values and their places are held back; the buttons keep their labels (owner, 2026-11-04).
    for word in ["SAMPLE_API_KEY", "wifi", "p-sample"] {
        assert!(!s.contains(word), "{word} in the tree: {s}");
    }
    let text = t.h.tree().text();
    assert!(text.contains("\"Add secret\""), "{text}");
    assert!(text.contains("(secret)"), "{text}");
}
