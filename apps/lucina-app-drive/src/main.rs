//! lucina-app-drive: Secretary's Drive on Juno (`web/src/views/Drive.tsx`), as a program of its own on the toolkit.
//! `LUCINA_APP_SAMPLE=1` runs on a made-up library in the process, with no account, and the header says so.
//! `--render <file> [folder|search|text|code|archive|picture|video|save]` writes the screen as a picture on the host.

// Juno with no C library: Eyra is the libc and the program start, in Rust (docs/research/juno-pure-rust.md).
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"))]
extern crate eyra;

mod account;
mod app;
mod intents;
mod server;
#[cfg(test)]
mod tests;

use std::sync::{Arc, mpsc};

use app::{Drive, Link};
use lucina_account::ui::SignInView;
use lucina_app::{Harness, Sender};
use lucina_app_support_now as now;
use lucina_files::fake::Fake;
use lucina_files::media::{Connect, system_connect};
use server::{Job, Msg, Worker};

mod lucina_app_support_now {
    pub fn ms() -> i64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
    }
}

/// One link: a thread that sends one request at a time.
fn start_worker(tx: Sender<Msg>, connect: Connect) -> Option<Link> {
    let media = connect()?;
    let (jobs, rx) = mpsc::channel::<Job>();
    let again = jobs.clone();
    let sender = tx.clone();
    std::thread::spawn(move || {
        let mut w = Worker::new(media, connect, Box::new(move |m| sender.send(m))).with_again(Box::new(move |j| {
            let _ = again.send(j);
        }));
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
    let fake = Fake::new(now::ms(), 8);
    let connect = fake.connect();
    let media = connect().expect("the sample library");
    let mut w = Worker::new(media, connect.clone(), Box::new(|_| {}));
    let (jobs, rx) = mpsc::channel::<Job>();
    let link: Link = Box::new(move |j| {
        let _ = jobs.send(j);
    });
    let zone = lucina_app::time::Zone::utc();
    let mut h = Harness::new(Drive::new(zone, None, Some(link), Some(connect)).sample(), 1920, 1080);
    let mut pump = |h: &mut Harness<Drive>| {
        h.core.input(lucina_app::Input::Wheel(0));
        h.core.paint();
        while let Ok(job) = rx.try_recv() {
            for m in w.handle(job) {
                h.core.message(m);
            }
        }
        h.core.input(lucina_app::Input::Wheel(0));
        h.core.paint();
    };
    h.app().start();
    pump(&mut h);
    let open = |h: &mut Harness<Drive>, name: &str| {
        let item = h.app().files.iter().find(|f| f.name() == name).cloned();
        if let Some(item) = item {
            h.app().open_preview(item);
        }
    };
    if what != "roots" && !what.is_empty() {
        h.app().open_folder("Lezioni".into(), String::new());
        pump(&mut h);
    }
    match what {
        "search" => {
            h.app().filters.q = "plan".into();
            h.app().search.set("plan");
            h.app().reload();
        }
        "text" => {
            h.app().open_folder("Lezioni".into(), "Digital Forensics".into());
            pump(&mut h);
            open(&mut h, "notes.md");
        }
        "code" => {
            h.app().open_folder("Lezioni".into(), "Digital Forensics".into());
            pump(&mut h);
            open(&mut h, "carve.rs");
        }
        "archive" => {
            h.app().open_folder("Lezioni".into(), "Digital Forensics".into());
            pump(&mut h);
            open(&mut h, "disk-images.zip");
        }
        "picture" => open(&mut h, "whiteboard.jpg"),
        "video" => open(&mut h, "lecture-recording.mp4"),
        _ => {}
    }
    pump(&mut h);
    std::fs::write(path, h.ppm()).expect("write");
}

fn main() {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some("--render") {
        let path = args.next().unwrap_or_else(|| "drive.ppm".into());
        render(&path, &args.next().unwrap_or_default());
        return;
    }
    lucina_app::run("lucina-app-drive", |tx| {
        let (_, zone) = lucina_app::time::load();
        if std::env::var_os("LUCINA_APP_SAMPLE").is_some() {
            let connect = Fake::new(now::ms(), 8).connect();
            let link = start_worker(tx.clone(), connect.clone());
            let mut app = Drive::new(zone, Some(tx), link, Some(connect)).sample();
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
        let mut app = Drive::new(zone, Some(tx), link, Some(connect)).with_sign_in(sign_in, worker);
        app.start();
        app
    });
}
