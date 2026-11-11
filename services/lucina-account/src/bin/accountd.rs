//! `lucina-accountd`: the system service that holds the sign-in to the owner's Secretary account ([`lucina_account::service`]).
//!
//! ```text
//! lucina-accountd [--keep-on-disk] [--root <dir>]
//! ```
//! `--keep-on-disk` is the owner's decision, not a default: without it the token stays in RAM, and a token on /data is removed.
//! `--root` runs it under another folder, for demos.
// Juno with no C library: Eyra is the libc and the program start, in Rust (docs/research/juno-pure-rust.md).
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"))]
extern crate eyra;

use std::path::PathBuf;

use lucina_account::service::{start, Config};

fn main() {
    let mut keep_on_disk = false;
    let mut root = PathBuf::from("/");
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match (a.as_str(), args.len()) {
            ("--keep-on-disk", _) => keep_on_disk = true,
            ("--root", 1..) => root = args.next().map(PathBuf::from).unwrap_or(root),
            _ => {
                eprintln!("usage: lucina-accountd [--keep-on-disk] [--root <dir>]");
                std::process::exit(2);
            }
        }
    }
    match start(Config::under(&root, keep_on_disk)) {
        Ok(running) => eprintln!("lucina-accountd: answering on {}, token on disk: {keep_on_disk}", running.socket().display()),
        Err(e) => {
            eprintln!("lucina-accountd: {e}");
            std::process::exit(1);
        }
    }
    loop {
        std::thread::park();
    }
}
