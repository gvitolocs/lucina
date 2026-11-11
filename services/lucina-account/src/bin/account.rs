//! `lucina-account`: Juno's account connection from a terminal, for checks and for signing in without an app.
//!
//! ```text
//! lucina-account health     GET /v1/health: DNS, TLS, roots and the clock in one request
//! lucina-account status     what lucina-accountd says, and whether the server still takes the token
//! lucina-account qr-check   makes a sign-in request and polls it once, without anyone approving. It expires by itself
//! lucina-account signin     shows the machine's QR and code in the terminal and waits for the phone
//! lucina-account signout    signs the machine out for every program. The device stays on the server until it is revoked there
//! ```
//! Never prints the token or the poll token.
// Juno with no C library: Eyra is the libc and the program start, in Rust (docs/research/juno-pure-rust.md).
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"))]
extern crate eyra;

use std::time::{Duration, Instant};

use lucina_account::signin::{Phase, Qr, SignIn, DEVICE_NAME, QUIET};
use lucina_account::system::{self, Link, Pairing, Status};
use lucina_account::Client;

fn draw_qr(qr: &Qr) {
    let w = qr.width + 2 * QUIET;
    let dark = |x: usize, y: usize| x >= QUIET && y >= QUIET && qr.is_dark(x - QUIET, y - QUIET);
    for y in (0..w).step_by(2) {
        let line: String = (0..w)
            .map(|x| match (dark(x, y), dark(x, y + 1)) {
                (false, false) => '█',
                (true, false) => '▄',
                (false, true) => '▀',
                (true, true) => ' ',
            })
            .collect();
        println!("{line}");
    }
}

fn main() {
    let cmd = std::env::args().nth(1).unwrap_or_default();
    let code = match cmd.as_str() {
        "health" => {
            let Some(c) = Client::from_env(None) else { return bad_base() };
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
            println!("clock: unix {now}");
            match c.health() {
                Ok(v) => {
                    println!("ok: {v}");
                    0
                }
                Err(e) => {
                    println!("failed: {e}");
                    1
                }
            }
        }
        "status" => status(),
        "qr-check" => {
            let Some(c) = Client::from_env(None) else { return bad_base() };
            match c.qr_start(DEVICE_NAME) {
                Ok(r) => {
                    println!("request made: expires in {} s, six digits: {}, link host: {}", r.expires_in, r.short_code.is_some(), r.approve_url.split('/').nth(2).unwrap_or("?"));
                    let mut s = SignIn::new(DEVICE_NAME);
                    let _ = s.tick(&c, Instant::now());
                    match s.phase(Instant::now()) {
                        Phase::Ready { qr, seconds_left, .. } => println!("second request made: QR {0}x{0} modules, {seconds_left} s left; nobody approved it", qr.width),
                        other => println!("{other:?}"),
                    }
                    0
                }
                Err(e) => {
                    println!("failed: {e}");
                    1
                }
            }
        }
        "signin" => signin(),
        "signout" => match Link::system().sign_out() {
            Ok(()) => {
                println!("signed out for every program; revoke the device on the server to end it");
                0
            }
            Err(e) => {
                println!("the account service does not answer: {e}");
                1
            }
        },
        _ => {
            eprintln!("usage: lucina-account health|status|qr-check|signin|signout");
            2
        }
    };
    std::process::exit(code);
}

fn status() -> i32 {
    let status = match Link::system().status() {
        Ok(s) => s,
        Err(e) => {
            println!("the account service does not answer: {e}");
            return 1;
        }
    };
    println!("{status:?}");
    let Some(client) = system::token().and_then(|t| Client::from_env(Some(t))) else { return 1 };
    match client.session() {
        Ok(s) if s.authenticated => {
            println!("the server takes the token: device {} ({})", s.device_id.as_deref().unwrap_or("?"), s.device_name.as_deref().unwrap_or("?"));
            0
        }
        Ok(_) => {
            println!("the server does not take the token");
            1
        }
        Err(e) => {
            println!("{e}");
            1
        }
    }
}

fn signin() -> i32 {
    let mut watch = match Link::system().watch(true) {
        Ok(w) => w,
        Err(e) => {
            println!("the account service does not answer: {e}");
            return 1;
        }
    };
    loop {
        let status = match watch.next(Duration::from_secs(1)) {
            Ok(Some(s)) => s,
            Ok(None) => continue,
            Err(e) => {
                println!("the account service went away: {e}");
                return 1;
            }
        };
        match status {
            Status::SignedIn { .. } => {
                println!("signed in");
                return 0;
            }
            Status::SigningIn { pairing: Pairing::Ready { approve_url, short_code, seconds_left } } => {
                if let Some(qr) = Qr::encode(&approve_url) {
                    draw_qr(&qr);
                }
                println!("Scan it in Secretary, or type {} there. {seconds_left} s.", short_code.as_deref().unwrap_or("the link"));
            }
            Status::SigningIn { pairing: Pairing::Denied } => {
                println!("declined on the phone");
                return 1;
            }
            Status::SigningIn { pairing: Pairing::Paused } => {
                println!("no answer to {} codes", lucina_account::signin::MAX_CODES);
                return 1;
            }
            Status::SigningIn { pairing: Pairing::Unavailable { message, .. } } => println!("{message}"),
            _ => {}
        }
    }
}

fn bad_base() {
    eprintln!("{} must be https, or http to this machine", lucina_account::client::BASE_ENV);
    std::process::exit(2);
}
