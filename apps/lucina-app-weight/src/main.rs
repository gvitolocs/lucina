//! lucina-app-weight: Secretary's Weight on Juno (`web/src/views/Weight.tsx`), as a program of its own on the toolkit.
//! `LUCINA_APP_SAMPLE=1` runs on an in-process engine with made-up weigh-ins and no account, and the top of the form says so.

// Juno with no C library: Eyra is the libc and the program start, in Rust (docs/research/juno-pure-rust.md).
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"))]
extern crate eyra;

mod account;
mod app;
mod server;
#[cfg(test)]
mod tests;

use std::sync::{Arc, mpsc};

use app::{Link, Weight};
use chrono_tz::Tz;
use lucina_account::ui::SignInView;
use lucina_account::credential;
use lucina_app::{Harness, Sender};
use secretary_body::{Engine, View};
use serde_json::json;
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

/// Ninety days of made-up weigh-ins, going down, and a goal.
pub fn sample_engine(now: u64) -> Engine {
    const DAY: u64 = 86_400_000;
    let mut e = Engine::in_memory();
    let mut send = |command: serde_json::Value, revision: u64, i: u64| {
        let r = json!({"op_id": format!("sample-{i}"), "expected_revision": revision, "timezone": "UTC", "command": command});
        e.handle_at(&r.to_string(), now);
    };
    send(json!({"type": "update_settings", "unit": "kg", "goal": "78"}), 0, 0);
    let mut revision = 1;
    for i in 0..30u64 {
        // A slow fall, with the day-to-day wobble of a real scale.
        let wobble = (i * 37 % 11) * 90;
        let grams = 84_400 - i * 150 + wobble;
        let note = match i {
            12 => "After the long run",
            24 => "Holiday week",
            _ => "",
        };
        let at = now - (30 - i) * 3 * DAY;
        send(
            json!({"type": "log_entry", "weight_grams": grams, "measured_ms": at, "note": note}),
            revision,
            i + 1,
        );
        revision += 1;
    }
    e
}

fn now_ms() -> u64 {
    secretary_body::now_ms()
}

/// `--render <file> [edit|settings]`: the screen as a picture, with the sample data.
fn render(path: &str, what: &str) {
    let tz = DEFAULT_TZ;
    let now = now_ms();
    let mut w = Worker::new(Box::new(Fake::new(sample_engine(now), now)), tz.name().to_owned());
    let (jobs, rx) = mpsc::channel::<Job>();
    let link: Link = Box::new(move |j| {
        let _ = jobs.send(j);
    });
    let mut h = Harness::new(Weight::new(tz, Some(link)).sample(), 1920, 1080);
    h.core.paint();
    while let Ok(job) = rx.try_recv() {
        for m in w.handle(job) {
            h.core.message(m);
        }
    }
    h.core.paint();
    match what {
        "edit" => {
            let e = h.app().view.as_ref().and_then(|v| v.entries.first().cloned());
            if let Some(e) = e {
                h.app().open_edit(e);
            }
        }
        "settings" => h.app().open_settings(),
        _ => {}
    }
    h.core.input(lucina_app::Input::Wheel(0));
    h.core.paint();
    h.core.input(lucina_app::Input::Wheel(0));
    h.core.paint();
    std::fs::write(path, h.ppm()).expect("write");
}

fn parse_cache(bytes: &[u8]) -> Option<(View, Option<String>)> {
    let v: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let view = serde_json::from_value(v.get("view")?.clone()).ok()?;
    let unit = v.get("unit").and_then(|u| u.as_str()).filter(|u| !u.is_empty()).map(str::to_owned);
    Some((view, unit))
}

fn main() {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some("--render") {
        let path = args.next().unwrap_or_else(|| "weight.ppm".into());
        render(&path, &args.next().unwrap_or_default());
        return;
    }
    lucina_app::run("lucina-app-weight", |tx| {
        let tz = timezone();
        if std::env::var_os("LUCINA_APP_SAMPLE").is_some() {
            let now = now_ms();
            let link = start_worker(tx, Box::new(Fake::new(sample_engine(now), now)), tz);
            let mut app = Weight::new(tz, Some(link)).sample();
            app.refresh();
            return app;
        }
        let (worker_tx, signin_tx) = (tx.clone(), tx.clone());
        let link = account_server().map(|s| start_worker(tx, s, tz));
        let signed = link.is_some();
        let sign_in: Box<dyn FnMut() -> Option<SignInView>> = Box::new(move || {
            let (a, b) = (signin_tx.clone(), signin_tx.clone());
            account::sign_in(Arc::new(move || a.send(Msg::Changed)), Arc::new(move || b.send(Msg::SignedIn)))
        });
        let worker: Box<dyn FnMut() -> Option<Link>> =
            Box::new(move || Some(start_worker(worker_tx.clone(), account_server()?, tz)));
        let mut app = Weight::new(tz, link).with_sign_in(sign_in, worker).with_cache(Box::new(|view, unit| {
            let saved = json!({"view": view, "unit": unit});
            let _ = credential::write_private(CACHE_FILE, saved.to_string().as_bytes());
        }));
        if signed {
            if let Some((view, unit)) = credential::read_private(CACHE_FILE).and_then(|b| parse_cache(&b)) {
                app.restore(view, unit);
            }
            app.refresh();
        }
        app
    });
}
