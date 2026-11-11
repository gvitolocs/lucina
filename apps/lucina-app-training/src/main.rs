//! lucina-app-training: Secretary's Training on Juno (`web/src/views/Training.tsx`, `web/src/views/gym/*`), as a program of
//! its own on the toolkit. `LUCINA_APP_SAMPLE=1` runs on in-process engines with made-up workouts and no account, and the
//! left column says so.

// Juno with no C library: Eyra is the libc and the program start, in Rust (docs/research/juno-pure-rust.md).
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"))]
extern crate eyra;

mod account;
mod app;
mod screens;
mod server;
#[cfg(test)]
mod tests;

use std::sync::{Arc, mpsc};

use app::{Link, Screen, Training};
use chrono_tz::Tz;
use lucina_account::credential;
use lucina_account::ui::SignInView;
use lucina_app::{Harness, Sender};
use secretary_gym::View;
use serde_json::{Value, json};
use server::{Account, CACHE_FILE, Fake, Job, Msg, Server, Worker};

const DEFAULT_TZ: Tz = chrono_tz::Europe::Copenhagen;

fn timezone() -> Tz {
    std::fs::read_to_string("/etc/lucina/timezone").ok().and_then(|s| s.trim().parse().ok()).unwrap_or(DEFAULT_TZ)
}

/// One link: a thread that sends one job at a time, so that each command sees the revision of the one before.
fn start_worker(tx: Sender<Msg>, server: Box<dyn Server>, tz: Tz) -> Link {
    let (jobs, rx) = mpsc::channel::<Job>();
    std::thread::spawn(move || {
        let mut w = Worker::new(server, tz.name().to_owned());
        while let Ok(job) = rx.recv() {
            for m in w.handle(job) {
                tx.send(m);
            }
        }
    });
    Box::new(move |j| {
        let _ = jobs.send(j);
    })
}

fn account_server() -> Option<Box<dyn Server>> {
    Some(Box::new(Account { client: account::client()? }))
}

/// One command on the sample's engine: what comes back, and the revision moved on.
fn sample_send(e: &mut secretary_gym::Engine, command: Value, at: u64) -> secretary_gym::Response {
    let body = json!({
        "op_id": format!("sample-{}", e.state().revision),
        "expected_revision": e.state().revision,
        "timezone": "UTC",
        "command": command,
    });
    e.handle_at(&body.to_string(), at)
}

/// Two more routines beside the engine's own, and eight finished workouts over four weeks, getting a little stronger.
pub fn sample_engine(now: u64) -> secretary_gym::Engine {
    const DAY: u64 = 86_400_000;
    let mut e = secretary_gym::Engine::in_memory();
    let ids: Vec<String> = e
        .state()
        .exercises
        .iter()
        .filter(|x| x.kind == secretary_gym::ExerciseKind::WeightReps)
        .take(3)
        .map(|x| x.id.clone())
        .collect();
    let at = now.saturating_sub(30 * DAY);
    sample_send(&mut e, json!({"type": "save_routine", "name": "Push day", "exercise_ids": ids}), at);
    let push = e.state().routines.iter().find(|r| r.name == "Push day").map(|r| r.id.clone()).unwrap_or_default();
    let second: Vec<&String> = ids.iter().rev().take(2).collect();
    sample_send(&mut e, json!({"type": "save_routine", "name": "Short session", "exercise_ids": second}), at);
    for w in 0..8u64 {
        let at = now.saturating_sub((28 - w * 3) * DAY);
        sample_send(&mut e, json!({"type": "start_workout", "title": "Push day", "routine_id": push}), at);
        let sets: Vec<String> = e
            .state()
            .active
            .iter()
            .flat_map(|a| a.exercises.iter())
            .flat_map(|x| x.sets.iter())
            .map(|s| s.id.clone())
            .collect();
        for (i, set) in sets.iter().enumerate() {
            let weight = 40 + w * 2 + (i as u64 % 3) * 10;
            sample_send(
                &mut e,
                json!({"type": "edit_set", "set_id": set, "weight": weight.to_string(), "reps": 8,
                    "seconds": 0, "distance_meters": 0, "kind": "normal"}),
                at,
            );
            sample_send(&mut e, json!({"type": "complete_set", "set_id": set, "completed": true}), at);
        }
        sample_send(&mut e, json!({"type": "finish_workout"}), at + 2_700_000);
    }
    e
}

pub fn sample_body(now: u64) -> secretary_body::Engine {
    let mut e = secretary_body::Engine::in_memory();
    for (i, (grams, days)) in [(81_200u64, 9u64), (80_900, 6), (80_600, 3)].into_iter().enumerate() {
        let body = json!({"op_id": format!("sample-{i}"), "expected_revision": i, "timezone": "UTC",
            "command": {"type": "log_entry", "weight_grams": grams, "measured_ms": now - days * 86_400_000}});
        e.handle_at(&body.to_string(), now);
    }
    e
}

/// `--render <file> [home|workout|journal|detail|progress|settings|picker|builder]`: a screen as a picture, on the sample.
fn render(path: &str, what: &str) {
    let tz = DEFAULT_TZ;
    let now = secretary_gym::now_ms();
    let fake = Fake::new(sample_engine(now), sample_body(now), now);
    let mut w = Worker::new(Box::new(fake), tz.name().to_owned());
    let (jobs, rx) = mpsc::channel::<Job>();
    let link: Link = Box::new(move |j| {
        let _ = jobs.send(j);
    });
    let mut h = Harness::new(Training::new(tz, Some(link)).sample(), 1920, 1080);
    let mut pump = |h: &mut Harness<Training>| {
        while let Ok(job) = rx.try_recv() {
            for m in w.handle(job) {
                h.core.message(m);
            }
        }
        h.core.paint();
    };
    h.core.paint();
    pump(&mut h);
    match what {
        "workout" | "picker" => {
            let (id, name) = {
                let v = h.app().view.as_ref().expect("view");
                (v.state.routines[0].id.clone(), v.state.routines[0].name.clone())
            };
            h.app().start_routine(&id, &name);
            pump(&mut h);
            if what == "picker" {
                h.app().open_picker(app::PickFor::Workout);
            }
        }
        "journal" => h.app().go(Screen::Journal),
        "detail" => {
            let id = h.app().view.as_ref().expect("view").state.history[0].id.clone();
            h.app().go(Screen::Detail(id));
        }
        "progress" => h.app().go(Screen::Progress),
        "settings" => h.app().go(Screen::Settings),
        "builder" => h.app().open_builder(),
        _ => {}
    }
    for _ in 0..3 {
        h.core.input(lucina_app::Input::Wheel(0));
        pump(&mut h);
    }
    std::fs::write(path, h.ppm()).expect("write");
}

fn main() {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some("--render") {
        let path = args.next().unwrap_or_else(|| "training.ppm".into());
        render(&path, &args.next().unwrap_or_default());
        return;
    }
    lucina_app::run("lucina-app-training", |tx| {
        let tz = timezone();
        if std::env::var_os("LUCINA_APP_SAMPLE").is_some() {
            let now = secretary_gym::now_ms();
            let fake = Fake::new(sample_engine(now), sample_body(now), 0);
            let waker = tx.clone();
            let link = start_worker(tx, Box::new(fake), tz);
            let mut app = Training::new(tz, Some(link)).sample().with_files(Arc::new(move || waker.send(Msg::Changed)), None);
            app.refresh();
            return app;
        }
        let (worker_tx, signin_tx, waker) = (tx.clone(), tx.clone(), tx.clone());
        let link = account_server().map(|s| start_worker(tx, s, tz));
        let signed = link.is_some();
        let sign_in: Box<dyn FnMut() -> Option<SignInView>> = Box::new(move || {
            let (a, b) = (signin_tx.clone(), signin_tx.clone());
            account::sign_in(Arc::new(move || a.send(Msg::Changed)), Arc::new(move || b.send(Msg::SignedIn)))
        });
        let worker: Box<dyn FnMut() -> Option<Link>> =
            Box::new(move || Some(start_worker(worker_tx.clone(), account_server()?, tz)));
        let mut app = Training::new(tz, link).with_files(Arc::new(move || waker.send(Msg::Changed)), Some(lucina_files::media::system_connect())).with_sign_in(sign_in, worker).with_cache(Box::new(|view: &View| {
            if let Ok(bytes) = serde_json::to_vec(view) {
                let _ = credential::write_private(CACHE_FILE, &bytes);
            }
        }));
        if signed {
            let cached = credential::read_private(CACHE_FILE).and_then(|b| serde_json::from_slice::<View>(&b).ok());
            if let Some(view) = cached {
                app.restore(view);
            }
            app.refresh();
        }
        app
    });
}
