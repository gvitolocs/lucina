//! lucina-app-tasks: Secretary's Tasks as a native Juno program. Behaviour comes from SecretaryIOS
//! `web/src/views/Tasks.tsx` and `web/src/state/tasks.ts`; lists, dates and parsing come from the
//! engine `secretary-tasks`; nothing about it needs a browser.
//!
//! `--render <file.ppm> [today|inbox|upcoming|project|detail|date]` draws the sample without a device.
//! `LUCINA_APP_SAMPLE=1` starts on the invented sample data, for a look without an account.

// Juno with no C library: Eyra is the libc and the program start, in Rust (docs/research/juno-pure-rust.md).
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"))]
extern crate eyra;

mod account;
mod app;
mod cache;
mod sample;
mod server;
mod store;
#[cfg(test)]
mod tests;
mod worker;

use std::sync::{Arc, mpsc};

use chrono_tz::Tz;
use lucina_account::ui::SignInView;
use lucina_account::system;
use lucina_app::{Harness, Sender};

use app::{Link, Mode, Tasks};
use server::{Fake, Server};
use store::{Store, query};
use worker::{Job, Msg, Worker};

const DEFAULT_TZ: Tz = chrono_tz::Europe::Copenhagen;

/// `/etc/lucina/timezone` as an IANA name; the shell's default otherwise. A bare POSIX rule isn't a
/// name the server's engine could use.
fn timezone() -> Tz {
    std::fs::read_to_string("/etc/lucina/timezone")
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(DEFAULT_TZ)
}

fn start_worker(tx: Sender<Msg>, server: Box<dyn Server>, tz: Tz) -> Link {
    let (jobs, rx) = mpsc::channel::<Job>();
    std::thread::spawn(move || {
        Worker::new(server, tz.name().to_owned(), query("today", None)).run(rx, |m| tx.send(m));
    });
    Box::new(move |j| {
        let _ = jobs.send(j);
    })
}

fn sample_server(tz: Tz) -> Box<dyn Server> {
    Box::new(Fake::new(sample::engine(tz, secretary_tasks::now_ms())))
}

/// The account, when this machine holds a device token.
fn account_server() -> Option<Box<dyn Server>> {
    let client = system::client()?;
    Some(Box::new(account::Account { client }))
}

fn render(path: &str, what: &str) {
    let tz = DEFAULT_TZ;
    let mut w = Worker::new(
        sample_server(tz),
        tz.name().to_owned(),
        query("today", None),
    );
    let mut h = Harness::new(Tasks::new(Store::new(tz), Mode::Sample, None), 1920, 1080);
    let mut load = |h: &mut Harness<Tasks>, q| {
        for m in w.handle(Job::Load(q)) {
            h.core.message(m);
        }
    };
    let q = match what {
        "inbox" => query("inbox", None),
        "upcoming" => query("upcoming", None),
        "project" => query("project", Some("p4")),
        _ => query("today", None),
    };
    load(&mut h, q.clone());
    h.app().q = q;
    h.app().rebuild();
    if what == "detail" {
        let id = h.app().list.selected.and_then(|i| match &h.app().rows[i] {
            app::Row::Task { t, .. } => Some(t.id.clone()),
            _ => None,
        });
        if let Some(id) = id {
            h.app().open_detail(&id);
        }
    }
    if what == "date" {
        h.key(lucina_app::keys::code_for('d').map_or(0, |c| c.0));
    }
    h.core.input(lucina_app::Input::Wheel(0));
    h.core.paint();
    std::fs::write(path, h.ppm()).expect("write");
}

fn main() {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some("--render") {
        let path = args.next().unwrap_or_else(|| "tasks.ppm".into());
        render(&path, &args.next().unwrap_or_default());
        return;
    }
    lucina_app::run("lucina-app-tasks", |tx| {
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
            Tasks::new(
                store,
                Mode::Sample,
                Some(start_worker(tx, sample_server(tz), tz)),
            )
        } else if let Some(server) = account_server() {
            if let Some(saved) = cache::load() {
                store.restore(saved);
            }
            let link = start_worker(tx, server, tz);
            let today = query("today", None);
            for q in store.pending.clone() {
                link(Job::Command(q, today.clone()));
            }
            Tasks::new(store, Mode::Account, Some(link))
        } else {
            Tasks::new(store, Mode::NotSignedIn, None)
        };
        app.with_starters(sample_start, account_start, sign_in)
    });
}
