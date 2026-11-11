//! lucina-app-secrets: Secretary's Secrets on Juno (`web/src/views/Secrets.tsx`, `components/SecretCard.tsx`), as a program of
//! its own on the toolkit. It shows names and places and takes new values through the paste-and-OK card. It cannot show a value:
//! no route returns one, and the field is a password field with no way to reveal it.
//! `LUCINA_APP_SAMPLE=1` runs on made-up names and no account, and the title says so.

// Juno with no C library: Eyra is the libc and the program start, in Rust (docs/research/juno-pure-rust.md).
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"))]
extern crate eyra;

mod account;
mod app;
mod server;
#[cfg(test)]
mod tests;

use std::sync::Arc;

use app::{Place, Secrets};
use lucina_account::credential;
use lucina_app::Harness;
use server::{Account, Msg, Sample, Server, run, start_worker};

/// The last place used: names only, in RAM beside the token. Never a value.
const PLACE_FILE: &str = "secrets-place.json";

fn account_server() -> Option<Box<dyn Server>> {
    Some(Box::new(Account { client: account::client()? }))
}

fn read_place() -> Option<Place> {
    let v: serde_json::Value = serde_json::from_slice(&credential::read_private(PLACE_FILE)?).ok()?;
    Some(Place { project: v["project"].as_str()?.to_owned(), environment: v["environment"].as_str()?.to_owned(), path: v["path"].as_str()?.to_owned() })
}

/// `--render <file> [card|blank|project]`: the screen as a picture, with the sample data and a value that is only ever bullets.
fn render(path: &str, what: &str) {
    let mut server = Sample::new();
    let (tx, rx) = std::sync::mpsc::channel();
    let mut h = Harness::new(Secrets::new(Some(Box::new(move |j| {
        let _ = tx.send(j);
    }))).sample(), 1920, 1080);
    let pump = |h: &mut Harness<Secrets>, server: &mut Sample| {
        for _ in 0..4 {
            while let Ok(job) = rx.try_recv() {
                let m = run(server, job);
                h.core.message(m);
            }
            h.core.input(lucina_app::Input::Wheel(0));
            h.core.paint();
        }
    };
    h.app().start();
    pump(&mut h, &mut server);
    match what {
        "card" => h.type_str("fake-canary-value"),
        "blank" => {
            h.key(lucina_app::keys::KEY_ESC);
            pump(&mut h, &mut server);
            h.key(lucina_app::keys::KEY_A);
        }
        "project" => {
            h.key(lucina_app::keys::KEY_ESC);
            pump(&mut h, &mut server);
            h.key(lucina_app::keys::KEY_N);
        }
        "" => {
            h.key(lucina_app::keys::KEY_ESC);
        }
        _ => {}
    }
    pump(&mut h, &mut server);
    std::fs::write(path, h.ppm()).expect("write");
}

fn main() {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some("--render") {
        let path = args.next().unwrap_or_else(|| "secrets.ppm".into());
        render(&path, &args.next().unwrap_or_default());
        return;
    }
    lucina_app::run("lucina-app-secrets", |tx| {
        if std::env::var_os("LUCINA_APP_SAMPLE").is_some() {
            let mut a = Secrets::new(Some(start_worker(tx, Box::new(Sample::new())))).sample();
            a.start();
            return a;
        }
        let (worker_tx, signin_tx) = (tx.clone(), tx.clone());
        let link = account_server().map(|s| start_worker(tx, s));
        let signed = link.is_some();
        let sign_in: Box<dyn FnMut() -> lucina_account::ui::SignInView> = Box::new(move || {
            let (a, b) = (signin_tx.clone(), signin_tx.clone());
            account::sign_in(Arc::new(move || a.send(Msg::Changed)), Arc::new(move || b.send(Msg::SignedIn)))
        });
        let worker: Box<dyn FnMut() -> Option<server::Link>> = Box::new(move || Some(start_worker(worker_tx.clone(), account_server()?)));
        let save_place = Box::new(|p: &Place| {
            let v = serde_json::json!({"project": p.project, "environment": p.environment, "path": p.path});
            let _ = credential::write_private(PLACE_FILE, v.to_string().as_bytes());
        });
        let mut a = Secrets::new(link).with_sign_in(sign_in, worker).with_place(read_place(), save_place);
        if signed {
            a.start();
        }
        a
    });
}
