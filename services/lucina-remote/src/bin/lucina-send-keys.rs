//! Keys for a test on the laptop, with nobody at it: written to an evdev device, they reach every program reading it as real ones do
//! (kernel patch 0406). Not in the image: a test pushes it to `/tmp`.
//!
//! usage: lucina-send-keys <device> <code>...   each code an evdev key number, or `sleep:<ms>`

// Juno with no C library: Eyra is the libc and the program start, in Rust (docs/research/juno-pure-rust.md).
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"))]
extern crate eyra;

use std::fs::OpenOptions;
use std::io::Write;
use std::time::Duration;

const EV_SYN: u16 = 0;
const EV_KEY: u16 = 1;

fn event(type_: u16, code: u16, value: i32) -> [u8; 24] {
    let mut out = [0u8; 24];
    out[16..18].copy_from_slice(&type_.to_ne_bytes());
    out[18..20].copy_from_slice(&code.to_ne_bytes());
    out[20..24].copy_from_slice(&value.to_ne_bytes());
    out
}

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: lucina-send-keys <device> <code>...");
        std::process::exit(2);
    };
    let mut device = OpenOptions::new().write(true).open(&path).unwrap_or_else(|err| {
        eprintln!("lucina-send-keys: {path}: {err}");
        std::process::exit(1);
    });
    for arg in args {
        if let Some(ms) = arg.strip_prefix("sleep:") {
            std::thread::sleep(Duration::from_millis(ms.parse().unwrap_or(0)));
            continue;
        }
        let Ok(code) = arg.parse::<u16>() else {
            eprintln!("lucina-send-keys: not a key number: {arg}");
            std::process::exit(2);
        };
        for value in [1, 0] {
            let mut bytes = Vec::with_capacity(48);
            bytes.extend_from_slice(&event(EV_KEY, code, value));
            bytes.extend_from_slice(&event(EV_SYN, 0, 0));
            if let Err(err) = device.write_all(&bytes) {
                eprintln!("lucina-send-keys: {path}: {err}");
                std::process::exit(1);
            }
            std::thread::sleep(Duration::from_millis(60));
        }
    }
}
