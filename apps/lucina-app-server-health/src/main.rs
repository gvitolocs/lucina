//! lucina-app-server-health: Secretary's Server page on Juno (`web/src/views/ServerHealth.tsx`), as a program of its own on the
//! toolkit. `LUCINA_APP_SAMPLE=1` runs on a made-up document and no account, and the title says so.

// Juno with no C library: Eyra is the libc and the program start, in Rust (docs/research/juno-pure-rust.md).
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"))]
extern crate eyra;

mod account;
mod app;
mod model;
mod pages;
mod server;
#[cfg(test)]
mod tests;

use std::sync::{Arc, mpsc};
use std::time::{SystemTime, UNIX_EPOCH};

use app::{Link, Msg, ServerApp};
use lucina_account::credential;
use lucina_app::{Harness, Sender};
use server::{Account, CACHE_FILE, Fake, Server, sample_json};

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

/// One thread, one request at a time.
fn start_worker(tx: Sender<Msg>, mut server: Box<dyn Server>) -> Link {
    let (jobs, rx) = mpsc::channel::<bool>();
    std::thread::spawn(move || {
        while let Ok(fresh) = rx.recv() {
            tx.send(match server.overview(fresh) {
                Ok(o) => Msg::Loaded(Box::new(o)),
                Err(e) => Msg::Failed(e),
            });
        }
    });
    Box::new(move |fresh| {
        let _ = jobs.send(fresh);
    })
}

fn account_server() -> Option<Box<dyn Server>> {
    Some(Box::new(Account { client: account::client()? }))
}

/// `--render <file> [services|incidents|deploys|item|host|component]`: the screen as a picture, with the sample data.
fn render(path: &str, what: &str) {
    let now = now_ms();
    let doc = serde_json::from_value(sample_json(now)).expect("the sample parses");
    let (_, zone) = lucina_app::time::load();
    let mut a = ServerApp::new(zone, None).sample();
    a.restore(doc);
    let mut h = Harness::new(a, 1920, 1080);
    match what {
        "services" => h.key(lucina_app::keys::KEY_2),
        "incidents" => h.key(lucina_app::keys::KEY_3),
        "deploys" => h.key(lucina_app::keys::KEY_4),
        "item" => {
            h.app().act(pages::Act::OpenItem("catalog:gmaps-server".into()), None);
            Vec::new()
        }
        "host" => {
            h.app().act(pages::Act::OpenHost("buildhost".into()), None);
            Vec::new()
        }
        "component" => {
            h.app().act(pages::Act::OpenComponent("secretary-server".into()), None);
            Vec::new()
        }
        _ => Vec::new(),
    };
    h.core.input(lucina_app::Input::Wheel(0));
    h.core.paint();
    h.core.input(lucina_app::Input::Wheel(0));
    h.core.paint();
    std::fs::write(path, h.ppm()).expect("write");
}

fn main() {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some("--render") {
        let path = args.next().unwrap_or_else(|| "server.ppm".into());
        render(&path, &args.next().unwrap_or_default());
        return;
    }
    lucina_app::run("lucina-app-server-health", |tx| {
        let (_, zone) = lucina_app::time::load();
        if std::env::var_os("LUCINA_APP_SAMPLE").is_some() {
            let link = start_worker(tx, Box::new(Fake::new(sample_json(now_ms()))));
            let mut a = ServerApp::new(zone, Some(link)).sample();
            a.refresh(false);
            return a;
        }
        let (worker_tx, signin_tx) = (tx.clone(), tx.clone());
        let link = account_server().map(|s| start_worker(tx, s));
        let signed = link.is_some();
        let sign_in: Box<dyn FnMut() -> lucina_account::ui::SignInView> = Box::new(move || {
            let (a, b) = (signin_tx.clone(), signin_tx.clone());
            account::sign_in(Arc::new(move || a.send(Msg::Changed)), Arc::new(move || b.send(Msg::SignedIn)))
        });
        let worker: Box<dyn FnMut() -> Option<Link>> = Box::new(move || Some(start_worker(worker_tx.clone(), account_server()?)));
        let mut a = ServerApp::new(zone, link).with_sign_in(sign_in, worker).with_cache(Box::new(|o| {
            if let Ok(bytes) = serde_json::to_vec(o) {
                let _ = credential::write_private(CACHE_FILE, &bytes);
            }
        }));
        if signed {
            if let Some(o) = credential::read_private(CACHE_FILE).and_then(|b| serde_json::from_slice(&b).ok()) {
                a.restore(o);
            }
            a.refresh(false);
        }
        a
    });
}
