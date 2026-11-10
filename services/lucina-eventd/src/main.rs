// Juno with no C library: Eyra is the libc and the program start, in Rust (docs/research/juno-pure-rust.md).
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"))]
extern crate eyra;

// lucina-eventd: the one event queue. Listens on the program socket and the root read socket, reads the kernel device.
//
// With a command it is a reader instead, for a person or a script on the machine (root, as the read socket wants):
//   lucina-eventd wait --for media.ended [--for download.*] [--app video] [--subject item:] [--since N] [--timeout-ms 60000]
//   lucina-eventd read [--since N] [--max 64] [--for ...] [--app ...]
//   lucina-eventd state | stats
// Each prints the hub's answer as one JSON line. `wait` exits 0 with the event, 2 on a timeout.

use std::path::PathBuf;
use std::sync::Arc;

use lucina_events::Reader;
use lucina_events::hub::{self, Config, Hub};
use lucina_events::wire::{self, Filter, Source};

fn read_socket() -> PathBuf {
    std::env::var("LUCINA_EVENTS_READ_SOCKET").unwrap_or_else(|_| wire::READ_SOCKET.into()).into()
}

fn usage() -> ! {
    eprintln!(
        "usage: lucina-eventd [wait|read|state|stats] [--for KIND]... [--app ID] [--src app|toolkit|shell|kernel]... [--subject PREFIX] [--since N] [--max N] [--timeout-ms N]"
    );
    std::process::exit(64)
}

struct Args {
    filter: Filter,
    since: u64,
    max: u64,
    timeout_ms: u64,
}

fn parse(mut rest: impl Iterator<Item = String>) -> Args {
    let mut a = Args { filter: Filter::default(), since: 0, max: 64, timeout_ms: 60_000 };
    while let Some(flag) = rest.next() {
        let value = rest.next().unwrap_or_else(|| usage());
        let number = || value.parse::<u64>().unwrap_or_else(|_| usage());
        match flag.as_str() {
            "--for" => a.filter.kind.push(value.clone()),
            "--app" => a.filter.app = Some(value.clone()),
            "--subject" => a.filter.subject_prefix = Some(value.clone()),
            "--src" => a.filter.src.push(match value.as_str() {
                "app" => Source::App,
                "toolkit" => Source::Toolkit,
                "shell" => Source::Shell,
                "kernel" => Source::Kernel,
                _ => usage(),
            }),
            "--since" => a.since = number(),
            "--max" => a.max = number(),
            "--timeout-ms" => a.timeout_ms = number(),
            _ => usage(),
        }
    }
    if let Err(e) = a.filter.check() {
        eprintln!("lucina-eventd: {e}");
        std::process::exit(64);
    }
    a
}

fn print(v: impl serde::Serialize) {
    println!("{}", serde_json::to_string(&v).unwrap_or_default());
}

fn reader(command: &str, rest: impl Iterator<Item = String>) -> ! {
    let a = parse(rest);
    let mut r = Reader::connect(&read_socket()).unwrap_or_else(|e| {
        eprintln!("lucina-eventd: the hub does not answer on {} ({e})", read_socket().display());
        std::process::exit(1)
    });
    let done = match command {
        "wait" => r.wait(a.since, &a.filter, a.timeout_ms).map(|w| {
            let code = if w.timeout { 2 } else { 0 };
            print(w);
            code
        }),
        "read" => r.read(a.since, &a.filter, Some(a.max)).map(|b| {
            print(b);
            0
        }),
        "state" => r.state().map(|s| {
            print(s);
            0
        }),
        "stats" => r.stats().map(|s| {
            print(s);
            0
        }),
        _ => usage(),
    };
    match done {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("lucina-eventd: {e}");
            std::process::exit(1)
        }
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    if let Some(command) = args.next() {
        reader(&command, args);
    }
    let mut cfg = Config::default();
    if let Ok(p) = std::env::var("LUCINA_EVENTS_SOCKET") {
        cfg.socket = p.into();
    }
    cfg.read_socket = read_socket();
    if let Ok(v) = std::env::var("LUCINA_EVENTS_KERNEL") {
        cfg.kernel = if v == "off" { None } else { Some(v.into()) };
    }
    eprintln!("lucina-eventd: listening on {} (read {})", cfg.socket.display(), cfg.read_socket.display());
    let hub = Arc::new(Hub::new(cfg.clone(), hub::monotonic_ms));
    if let Err(e) = hub::serve(hub, &cfg) {
        eprintln!("lucina-eventd: {e}");
        std::process::exit(1);
    }
}
