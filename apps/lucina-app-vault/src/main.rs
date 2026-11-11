//! lucina-app-vault: an Obsidian vault on Juno, as a program of its own on the toolkit. The vault is a folder on `/data`
//! (`/data/vault`, or `LUCINA_VAULT_DIR`) that real Obsidian opens as it is. Everything about the vault is the `lucina-vault`
//! crate, shared with the Secretary apps and held against real Obsidian there. This crate is only the screens.

// Juno with no C library: Eyra is the libc and the program start, in Rust (docs/research/juno-pure-rust.md).
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"))]
extern crate eyra;

mod app;
mod doc;
mod graphview;
mod sync;
#[cfg(test)]
mod tests;
mod worker;

use std::path::PathBuf;
use std::sync::mpsc;

use app::VaultApp;
use worker::{Job, Worker};

/// Where the vault is expected. One folder holding a vault is good enough too.
fn vault_dir() -> PathBuf {
    if std::env::var_os("LUCINA_APP_SAMPLE").is_some() {
        return PathBuf::from("/usr/share/lucina/vault-sample");
    }
    if let Some(d) = std::env::var_os("LUCINA_VAULT_DIR") {
        return d.into();
    }
    let base = PathBuf::from("/data/vault");
    if base.join(".obsidian").exists() {
        return base;
    }
    let inner: Vec<PathBuf> = std::fs::read_dir(&base)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join(".obsidian").is_dir())
        .collect();
    match inner.as_slice() {
        [one] => one.clone(),
        _ => base,
    }
}

/// `--render <vault> <prefix> [note]`: the screens as PPM files, on the host, to look at without a device.
fn render(args: &[String]) {
    use lucina_app::{Harness, keys};
    let (jobs, rx) = mpsc::channel::<Job>();
    let again = jobs.clone();
    let mut w = Worker::new(std::env::temp_dir().join("lucina-vault-render"));
    let mut h = Harness::new(VaultApp::new(PathBuf::from(&args[0]), jobs), 1920, 1080);
    let mut pump = |h: &mut Harness<VaultApp>| {
        for _ in 0..4 {
            h.core.input(lucina_app::Input::Wheel(0));
            h.core.paint();
            while let Ok(job) = rx.try_recv() {
                let t = std::time::Instant::now();
                let name = match &job {
                    Job::Open(_) => "open",
                    Job::Note(_) => "note",
                    Job::Search { .. } => "search",
                    Job::Graph { .. } => "graph",
                    _ => "other",
                };
                let msgs = w.handle(job);
                eprintln!("{name}: {:?}", t.elapsed());
                for m in msgs {
                    h.core.message(m);
                }
            }
        }
        h.core.input(lucina_app::Input::Wheel(0));
        h.core.paint();
    };
    let shot = |h: &Harness<VaultApp>, name: &str| {
        std::fs::write(format!("{}{name}.ppm", args[1]), h.ppm()).expect("write");
    };
    pump(&mut h);
    shot(&h, "home");
    if let Some(n) = args.get(2) {
        let _ = again.send(Job::Note(n.clone()));
        pump(&mut h);
        shot(&h, "note");
    }
    h.key(keys::KEY_F3);
    pump(&mut h);
    h.type_str("the");
    pump(&mut h);
    shot(&h, "search");
    h.key(app::k::F4);
    pump(&mut h);
    shot(&h, "tags");
    h.key(keys::KEY_F2);
    h.key(app::k::F5);
    pump(&mut h);
    pump(&mut h);
    shot(&h, "graph");
    h.key(app::k::F6);
    pump(&mut h);
    pump(&mut h);
    shot(&h, "local");
    h.key(keys::KEY_ESC);
    let t = std::time::Instant::now();
    h.wheel(-3);
    eprintln!("wheel: {:?}", t.elapsed());
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|a| a == "--render") {
        render(&args[1..]);
        return;
    }
    lucina_app::run("lucina-app-vault", |tx| {
        let (jobs, rx) = mpsc::channel::<Job>();
        std::thread::spawn(move || {
            let mut w = Worker::new(PathBuf::from("/data/lucina/vault"));
            for job in rx {
                for m in w.handle(job) {
                    tx.send(m);
                }
            }
        });
        let root = vault_dir();
        // Only the vault at /data/vault follows the server; a sample or a folder named by LUCINA_VAULT_DIR stays as it is.
        if root == std::path::Path::new(sync::SYNCED_ROOT) {
            let ticker = jobs.clone();
            let _ = jobs.send(Job::Sync(PathBuf::from(sync::SYNCED_ROOT)));
            std::thread::spawn(move || loop {
                std::thread::sleep(sync::EVERY);
                if ticker.send(Job::Sync(PathBuf::from(sync::SYNCED_ROOT))).is_err() {
                    return;
                }
            });
        }
        VaultApp::new(root, jobs)
    });
}
