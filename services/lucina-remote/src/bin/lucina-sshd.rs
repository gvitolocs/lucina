//! lucina-sshd: the SSH server of LucinaOS (sunset). Key-only logins as `root`; one thread per connection.
//!
//! ```text
//! lucina-sshd [--listen ADDR] [--host-key FILE] [--authorized-keys FILE] [--shell PATH] [-v]
//! ```
//!
//! Defaults: `0.0.0.0:22`, `/etc/lucina/ssh_host_ed25519_key`, `/etc/lucina/authorized_keys`, `/bin/sh`.

// Juno with no C library: Eyra is the libc and the program start, in Rust (docs/research/juno-pure-rust.md).
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"))]
extern crate eyra;

use std::{net::TcpListener, process::ExitCode, sync::Arc};

use log::{error, info};
use lucina_remote::{
    conn::{self, Config},
    keys::{fingerprint, parse_authorized_keys, parse_openssh_ed25519_private},
};
use sunset::{SignKey, ed25519_dalek::SigningKey};

/// Waits until an interface other than loopback has an address.
///
/// LucinaOS binds a socket on an unspecified address to one interface, the first other than loopback that
/// has an address, or loopback (kernel patch 0200); a server started before DHCP has finished would listen on
/// loopback only. A probe bind shows which: its local address is the interface's. On Linux it stays
/// `0.0.0.0`, which passes at once.
fn wait_for_network() {
    let mut waited = 0u32;
    loop {
        let local = TcpListener::bind("0.0.0.0:0").and_then(|l| l.local_addr());
        match local {
            Ok(addr) if !addr.ip().is_loopback() => return,
            _ => {}
        }
        if waited.is_multiple_of(60) {
            info!("waiting for a network address");
        }
        waited += 1;
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

fn main() -> ExitCode {
    let mut listen = "0.0.0.0:22".to_string();
    let mut host_key = "/etc/lucina/ssh_host_ed25519_key".to_string();
    let mut authorized = "/etc/lucina/authorized_keys".to_string();
    let mut shell = "/bin/sh".to_string();
    let mut level = log::LevelFilter::Info;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut value = |name: &str| args.next().ok_or_else(|| format!("{name} needs a value"));
        let r = match a.as_str() {
            "--listen" => value("--listen").map(|v| listen = v),
            "--host-key" => value("--host-key").map(|v| host_key = v),
            "--authorized-keys" => value("--authorized-keys").map(|v| authorized = v),
            "--shell" => value("--shell").map(|v| shell = v),
            "-v" => {
                level = log::LevelFilter::Debug;
                Ok(())
            }
            _ => Err(format!("unknown argument {a:?}")),
        };
        if let Err(e) = r {
            eprintln!("lucina-sshd: {e}");
            return ExitCode::from(2);
        }
    }
    lucina_remote::log::init("lucina-sshd", level);

    let seed = match std::fs::read_to_string(&host_key)
        .map_err(|e| e.to_string())
        .and_then(|t| parse_openssh_ed25519_private(&t).map_err(|e| e.to_string()))
    {
        Ok(s) => s,
        Err(e) => {
            error!("host key {host_key}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let signing = SigningKey::from_bytes(&seed);
    let keys = match std::fs::read_to_string(&authorized) {
        Ok(t) => parse_authorized_keys(&t),
        Err(e) => {
            error!("{authorized}: {e}");
            return ExitCode::FAILURE;
        }
    };
    if keys.is_empty() {
        error!("{authorized} has no ssh-ed25519 key; nobody could log in");
        return ExitCode::FAILURE;
    }
    info!(
        "host key {}",
        fingerprint(&signing.verifying_key().to_bytes())
    );
    for k in &keys {
        info!("authorized key {} {}", k.fingerprint(), k.comment);
    }
    let cfg = Arc::new(Config {
        host_key: SignKey::Ed25519(signing),
        authorized: keys,
        shell,
    });

    if listen.starts_with("0.0.0.0:") {
        wait_for_network();
    }
    let listener = match TcpListener::bind(&listen) {
        Ok(l) => l,
        Err(e) => {
            error!("listen on {listen}: {e}");
            return ExitCode::FAILURE;
        }
    };
    match listener.local_addr() {
        Ok(addr) => info!("listening on {listen} ({addr})"),
        Err(_) => info!("listening on {listen}"),
    }
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(s) => s,
            Err(e) => {
                error!("accept: {e}");
                continue;
            }
        };
        let cfg = cfg.clone();
        let peer = stream
            .peer_addr()
            .map(|a| a.to_string())
            .unwrap_or_default();
        info!("{peer}: connected");
        let thread_peer = peer.clone();
        let spawned = std::thread::Builder::new()
            .name(format!("ssh {peer}"))
            .spawn(move || {
                if let Err(e) = conn::serve(stream, &cfg) {
                    info!("{thread_peer}: {e}");
                }
            });
        if let Err(e) = spawned {
            error!("{peer}: no thread: {e}");
        }
    }
    ExitCode::SUCCESS
}
