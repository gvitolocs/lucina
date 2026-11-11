//! lucina-app-library: Secretary's Library on Juno (`web/src/views/Library.tsx`), as a program of its own on the toolkit.
//! `LUCINA_APP_SAMPLE=1` runs on an in-process copy of the routes with made-up books and no account, and the left column says so.

// Juno with no C library: Eyra is the libc and the program start, in Rust (docs/research/juno-pure-rust.md).
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"))]
extern crate eyra;

mod account;
mod app;
mod fake;
mod server;
#[cfg(test)]
mod tests;

use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use app::{Library, Link};
use chrono_tz::Tz;
use fake::Fake;
use lucina_account::ui::SignInView;
use lucina_app::{Harness, Sender};
use server::{Account, Job, Msg, Server, Worker};

const DEFAULT_TZ: Tz = chrono_tz::Europe::Copenhagen;

fn timezone() -> Tz {
    std::fs::read_to_string("/etc/lucina/timezone").ok().and_then(|s| s.trim().parse().ok()).unwrap_or(DEFAULT_TZ)
}

fn start_worker(tx: Sender<Msg>, server: Box<dyn Server>) -> Link {
    let (jobs, rx) = mpsc::channel::<Job>();
    std::thread::spawn(move || {
        let mut w = Worker::new(server);
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

/// `--render <file> [activity|search]`: the screen as a picture, on the sample.
fn render(path: &str, what: &str) {
    let mut w = Worker::new(Box::new(Fake::new(fake::sample())));
    let (jobs, rx) = mpsc::channel::<Job>();
    let link: Link = Box::new(move |j| {
        let _ = jobs.send(j);
    });
    let mut h = Harness::new(Library::new(DEFAULT_TZ, Some(link)).sample(), 1920, 1080);
    let mut pump = |h: &mut Harness<Library>| {
        h.core.input(lucina_app::Input::Wheel(0));
        h.core.paint();
        while let Ok(job) = rx.try_recv() {
            for m in w.handle(job) {
                h.core.message(m);
            }
        }
        h.core.paint();
    };
    pump(&mut h);
    let later = Instant::now() + Duration::from_secs(5);
    h.app().follow_selection();
    h.app().ask_if_settled(later);
    pump(&mut h);
    match what {
        "activity" => h.app().show_activity(),
        "search" => {
            h.app().query.set("quiet");
            h.app().reload();
        }
        _ => {}
    }
    pump(&mut h);
    pump(&mut h);
    std::fs::write(path, h.ppm()).expect("write");
}

fn main() {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some("--render") {
        let path = args.next().unwrap_or_else(|| "library.ppm".into());
        render(&path, &args.next().unwrap_or_default());
        return;
    }
    lucina_app::run("lucina-app-library", |tx| {
        let tz = timezone();
        if std::env::var_os("LUCINA_APP_SAMPLE").is_some() {
            let link = start_worker(tx, Box::new(Fake::new(fake::sample())));
            return Library::new(tz, Some(link)).sample();
        }
        let (worker_tx, signin_tx) = (tx.clone(), tx.clone());
        let link = account_server().map(|s| start_worker(tx, s));
        let sign_in: Box<dyn FnMut() -> Option<SignInView>> = Box::new(move || {
            let (a, b) = (signin_tx.clone(), signin_tx.clone());
            account::sign_in(Arc::new(move || a.send(Msg::Changed)), Arc::new(move || b.send(Msg::SignedIn)))
        });
        let worker: Box<dyn FnMut() -> Option<Link>> =
            Box::new(move || Some(start_worker(worker_tx.clone(), account_server()?)));
        Library::new(tz, link).with_sign_in(sign_in, worker)
    });
}
