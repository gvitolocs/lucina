//! lucina-app-gallery: Secretary's Gallery on Juno (`web/src/views/Gallery.tsx`), as a program of its own on the toolkit: thumbnails first,
//! small in memory, its pictures cached on `/data`.
//! `LUCINA_APP_SAMPLE=1` runs on a made-up library in the process, with no account, and the header says so.
//! `--render <file> [viewer|info|albums|delete|trash|months]` writes the screen as a picture on the host.

// Juno with no C library: Eyra is the libc and the program start, in Rust (docs/research/juno-pure-rust.md).
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"))]
extern crate eyra;

mod account;
mod app;
mod intents;
mod model;
mod server;
#[cfg(test)]
mod tests;
mod viewer;

use std::sync::{Arc, mpsc};
use std::time::Duration;

use app::{Gallery, Link};
use lucina_account::ui::SignInView;
use lucina_app::{Harness, Sender};
use lucina_app_support::dates;
use lucina_files::fake::Fake;
use lucina_files::media::{Connect, system_connect};
use lucina_files::pictures::Pictures;
use server::{Job, Msg, Worker};

/// One link: a thread that sends one request at a time.
fn start_worker(tx: Sender<Msg>, connect: Connect) -> Option<Link> {
    let media = connect()?;
    let (jobs, rx) = mpsc::channel::<Job>();
    let sender = tx.clone();
    std::thread::spawn(move || {
        let mut w = Worker::new(media, connect, Arc::new(move |m| sender.send(m)));
        while let Ok(job) = rx.recv() {
            for m in w.handle(job) {
                tx.send(m);
            }
        }
    });
    Some(Box::new(move |j| {
        let _ = jobs.send(j);
    }))
}

/// The screen as a picture, on the sample library.
fn render(path: &str, what: &str) {
    let fake = Fake::new(dates::now_ms(), 96);
    let connect = fake.connect();
    let media = connect().expect("the sample library");
    let done: Arc<std::sync::Mutex<Vec<lucina_files::pictures::Done>>> = Default::default();
    let mut w = Worker::new(media, connect.clone(), Arc::new(|_| {}));
    let (jobs, rx) = mpsc::channel::<Job>();
    let link: Link = Box::new(move |j| {
        let _ = jobs.send(j);
    });
    let zone = lucina_app::time::Zone::utc();
    let mut h = Harness::new(Gallery::new(zone, None, Some(link), None).sample(), 1920, 1080);
    let sink = done.clone();
    h.app().pictures = Some(Pictures::start(connect, None, move |d| sink.lock().unwrap().push(d)));
    h.app().start();
    let mut pump = |h: &mut Harness<Gallery>| {
        for _ in 0..30 {
            h.core.input(lucina_app::Input::Wheel(0));
            h.core.paint();
            while let Ok(job) = rx.try_recv() {
                for m in w.handle(job) {
                    h.core.message(m);
                }
            }
            for d in done.lock().unwrap().drain(..) {
                h.core.message(Msg::Picture(d));
            }
            std::thread::sleep(Duration::from_millis(40));
        }
    };
    pump(&mut h);
    match what {
        "viewer" | "info" => {
            h.app().focus = 5;
            h.app().open_viewer(5);
            pump(&mut h);
            if what == "info" {
                h.key(lucina_app::keys::KEY_I);
                pump(&mut h);
            }
        }
        "zoom" => {
            h.app().open_viewer(1);
            pump(&mut h);
            h.key(lucina_app::keys::KEY_ENTER);
            pump(&mut h);
            h.key(lucina_app::keys::KEY_RIGHT);
            pump(&mut h);
        }
        "albums" => {
            h.key(lucina_app::keys::KEY_A);
            pump(&mut h);
        }
        "delete" => {
            h.key(lucina_app::keys::KEY_DELETE);
            pump(&mut h);
        }
        "select" => {
            h.key(lucina_app::keys::KEY_SPACE);
            h.key(lucina_app::keys::KEY_RIGHT);
            h.key(lucina_app::keys::KEY_SPACE);
            pump(&mut h);
        }
        "months" => {
            h.key(lucina_app::keys::KEY_G);
            pump(&mut h);
        }
        _ => {}
    }
    std::fs::write(path, h.ppm()).expect("write");
}

fn main() {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some("--render") {
        let path = args.next().unwrap_or_else(|| "gallery.ppm".into());
        render(&path, &args.next().unwrap_or_default());
        return;
    }
    lucina_app::run("lucina-app-gallery", |tx| {
        let (_, zone) = lucina_app::time::load();
        if std::env::var_os("LUCINA_APP_SAMPLE").is_some() {
            let connect = Fake::new(dates::now_ms(), 240).connect();
            let link = start_worker(tx.clone(), connect.clone());
            let mut app = Gallery::new(zone, Some(tx), link, Some(connect)).sample();
            app.start();
            return app;
        }
        let connect = system_connect();
        let (worker_tx, signin_tx) = (tx.clone(), tx.clone());
        let link = start_worker(tx.clone(), connect.clone());
        let sign_in: Box<dyn FnMut() -> Option<SignInView>> = Box::new(move || {
            let (a, b) = (signin_tx.clone(), signin_tx.clone());
            account::sign_in(Arc::new(move || a.send(Msg::Changed)), Arc::new(move || b.send(Msg::SignedIn)))
        });
        let worker_connect = connect.clone();
        let worker: Box<dyn FnMut() -> Option<Link>> = Box::new(move || start_worker(worker_tx.clone(), worker_connect.clone()));
        let mut app = Gallery::new(zone, Some(tx), link, Some(connect)).with_sign_in(sign_in, worker);
        app.start();
        app
    });
}
