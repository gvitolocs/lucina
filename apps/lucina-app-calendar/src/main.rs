//! lucina-app-calendar: Secretary's Calendar as a native Juno program. Behaviour comes from SecretaryIOS
//! `web/src/views/Calendar.tsx` and `web/src/lib/calendar.ts`; events are the engine `secretary-calendar`'s.
//!
//! The owner's events are `iphone:` ones, read-only on the server, so here they can be looked at and
//! never changed: only `editable` events get Edit and Delete, and a new event goes to Lucina's own
//! calendar, as in the web app.
//!
//! `--render <file.ppm> [month|week|day|detail|editor]` draws the sample without a device.
//! `LUCINA_APP_SAMPLE=1` starts on the invented sample data, for a look without an account.

// Juno with no C library: Eyra is the libc and the program start, in Rust (docs/research/juno-pure-rust.md).
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"))]
extern crate eyra;

mod account;
mod app;
mod sample;
mod server;
mod store;
#[cfg(test)]
mod tests;
mod worker;

use std::sync::{Arc, mpsc};

use chrono_tz::Tz;
use lucina_account::ui::SignInView;
use lucina_account::{credential, system};
use lucina_app::{Harness, Sender};

use app::{Calendar, Kind, Link, Mode};
use server::Server;
use store::Store;
use worker::{Job, Msg, Worker};

const DEFAULT_TZ: Tz = chrono_tz::Europe::Copenhagen;

/// `/etc/lucina/timezone` as an IANA name; the shell's default otherwise.
fn timezone() -> Tz {
    std::fs::read_to_string("/etc/lucina/timezone")
        .ok()
        .and_then(|s| secretary_calendar::parse_tz(&s))
        .unwrap_or(DEFAULT_TZ)
}

fn start_worker(tx: Sender<Msg>, server: Box<dyn Server>, tz: Tz) -> Link {
    let (jobs, rx) = mpsc::channel::<Job>();
    std::thread::spawn(move || {
        Worker::new(server, tz).run(rx, |m| tx.send(m));
    });
    Box::new(move |j| {
        let _ = jobs.send(j);
    })
}

fn sample_server(tz: Tz) -> Box<dyn Server> {
    let today = store::local(store::now_ms(), tz).date();
    Box::new(sample::server(tz, today))
}

/// The account, when this machine holds a device token.
fn account_server() -> Option<Box<dyn Server>> {
    let client = system::client()?;
    Some(Box::new(account::Account { client }))
}

fn render(path: &str, what: &str) {
    let tz = DEFAULT_TZ;
    let mut w = Worker::new(sample_server(tz), tz);
    let mut app = Calendar::new(Store::new(tz), Mode::Sample, None);
    app.kind = match what {
        "month" => Kind::Month,
        "day" => Kind::Day,
        _ => Kind::Week,
    };
    let months = app.visible_months();
    let mut h = Harness::new(app, 1920, 1080);
    for m in w.handle(Job::Visible(months)) {
        h.core.message(m);
    }
    if what == "detail" {
        let i = h
            .app()
            .store
            .events
            .iter()
            .position(|e| e.title == "Design review");
        if let Some(i) = i {
            h.app().open_event(i);
        }
    }
    if what == "editor" {
        h.app().new_event_default();
    }
    h.core.input(lucina_app::Input::Wheel(0));
    h.core.paint();
    h.core.input(lucina_app::Input::Wheel(0));
    h.core.paint();
    std::fs::write(path, h.ppm()).expect("write");
}

fn main() {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some("--render") {
        let path = args.next().unwrap_or_else(|| "calendar.ppm".into());
        render(&path, &args.next().unwrap_or_default());
        return;
    }
    lucina_app::run("lucina-app-calendar", |tx| {
        let tz = timezone();
        let mut store = Store::new(tz);
        let (sample_tx, account_tx, signin_tx) = (tx.clone(), tx.clone(), tx.clone());
        let sample_start: Box<dyn FnMut() -> Link> =
            Box::new(move || start_worker(sample_tx.clone(), sample_server(tz), tz));
        let account_start: Box<dyn FnMut() -> Option<Link>> =
            Box::new(move || Some(start_worker(account_tx.clone(), account_server()?, tz)));
        let sign_in: Box<dyn FnMut() -> Option<SignInView>> = Box::new(move || {
            let (a, b) = (signin_tx.clone(), signin_tx.clone());
            Some(SignInView::start(
                Arc::new(move || a.send(Msg::Changed)),
                Arc::new(move || b.send(Msg::SignedIn)),
            ))
        });
        let app = if std::env::var_os("LUCINA_APP_SAMPLE").is_some() {
            Calendar::new(
                store,
                Mode::Sample,
                Some(start_worker(tx, sample_server(tz), tz)),
            )
        } else if let Some(server) = account_server() {
            // Last time's months, in RAM: on screen before any network.
            if let Some(saved) = credential::read_private(account::CACHE_FILE)
                .and_then(|b| serde_json::from_slice(&b).ok())
            {
                store.months = saved;
            }
            Calendar::new(store, Mode::Account, Some(start_worker(tx, server, tz)))
        } else {
            Calendar::new(store, Mode::NotSignedIn, None)
        };
        app.with_starters(sample_start, account_start, sign_in)
    });
}
