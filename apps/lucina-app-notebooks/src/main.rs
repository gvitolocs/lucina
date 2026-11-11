//! lucina-app-notebooks: Secretary's Notebooks on Juno (`web/src/views/Notebooks.tsx`, `Notebook.tsx`, `notebooks/*`), as a program
//! of its own on the toolkit. `LUCINA_APP_SAMPLE=1` talks to `lucina-notebooks-sample` on this machine's loopback, with no account,
//! and the header says so: the real engine with made-up data, through the same code as the real server.

// Juno with no C library: Eyra is the libc and the program start, in Rust (docs/research/juno-pure-rust.md).
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"))]
extern crate eyra;

mod account;
mod app;
mod md;
mod screens;
mod server;
#[cfg(test)]
mod tests;
mod wire;

use std::sync::{Arc, mpsc};

use app::{Cached, Link, Notebooks};
use lucina_account::credential;
use lucina_account::ui::SignInView;
use lucina_account::Client;
use lucina_app::Sender;
use server::{CACHE_FILE, Job, Msg, handle};

const SAMPLE_BASE: &str = "http://127.0.0.1:8932";

/// One thread that sends one request at a time, and a thread for each reply being written, so a slow model holds up nothing else.
fn start_worker(tx: Sender<Msg>, client: Client) -> Link {
    let (jobs, rx) = mpsc::channel::<Job>();
    let (worker_tx, worker_client) = (tx.clone(), client.clone());
    std::thread::spawn(move || {
        while let Ok(job) = rx.recv() {
            handle(&worker_client, job, &mut |m| worker_tx.send(m));
        }
    });
    Box::new(move |job| {
        if job.is_long() {
            let (tx, client) = (tx.clone(), client.clone());
            std::thread::spawn(move || handle(&client, job, &mut |m| tx.send(m)));
        } else {
            let _ = jobs.send(job);
        }
    })
}

fn main() {
    lucina_app::run("lucina-app-notebooks", |tx| {
        let (_, zone) = lucina_app::time::load();
        if std::env::var_os("LUCINA_APP_SAMPLE").is_some() {
            let client = Client::new(SAMPLE_BASE, None).expect("loopback");
            let link = start_worker(tx, client);
            return Notebooks::new(zone, Some(link)).sample();
        }
        let (worker_tx, signin_tx) = (tx.clone(), tx.clone());
        let link = account::client().map(|c| start_worker(tx, c));
        let signed = link.is_some();
        let sign_in: Box<dyn FnMut() -> Option<SignInView>> = Box::new(move || {
            let (a, b) = (signin_tx.clone(), signin_tx.clone());
            account::sign_in(Arc::new(move || a.send(Msg::Changed)), Arc::new(move || b.send(Msg::SignedIn)))
        });
        let worker: Box<dyn FnMut() -> Option<Link>> =
            Box::new(move || Some(start_worker(worker_tx.clone(), account::client()?)));
        let mut app = Notebooks::new(zone, link).with_sign_in(sign_in, worker).with_cache(Box::new(|c: &Cached| {
            if let Ok(bytes) = serde_json::to_vec(c) {
                let _ = credential::write_private(CACHE_FILE, &bytes);
            }
        }));
        if signed {
            if let Some(c) = credential::read_private(CACHE_FILE).and_then(|b| serde_json::from_slice(&b).ok()) {
                app.restore(c);
            }
        }
        app
    });
}
