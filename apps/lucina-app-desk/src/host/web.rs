//! The host a browser opens: `--host --web`. lucina-desk-web speaks voxl's protocol, so any browser on this network sees and
//! drives this screen with voxl's web client, with nothing installed. As the RustDesk host's `--direct`, **the picture and the
//! input cross the network unencrypted**, so it listens only on private addresses and answers only the network of the address
//! a connection reached (or the one machine `--only` names), with a password made fresh from the kernel's randomness each
//! time it starts. The password is never logged or printed: it is in a file only root reads, in RAM, while the host runs,
//! and the app shows it on this laptop's own screen.

use std::io::Write;
use std::net::{IpAddr, SocketAddr};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::Duration;

use lucina_desk_web::{Config, Input, Listen, Screen, Server, net};

pub use lucina_desk_web::PORT;
/// What a browser needs, for the app to show. `LUCINA_DESK_WEB_FILE` puts it elsewhere, for tests.
pub const REACH_FILE: &str = "/run/lucina/desk-web";

/// What the running host left for the app: the password and the links that open this screen.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Reach {
    pub password: String,
    pub urls: Vec<String>,
}

pub fn reach_file() -> String {
    std::env::var("LUCINA_DESK_WEB_FILE").unwrap_or_else(|_| REACH_FILE.to_owned())
}

/// The link a browser opens: the password rides in it as voxl's `?token=`.
pub fn url(addr: SocketAddr, password: &str) -> String {
    format!("http://{addr}/?token={password}")
}

pub fn write_reach(path: &Path, reach: &Reach) -> Result<(), String> {
    let _ = std::fs::remove_file(path);
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let mut text = format!("password = {}\n", reach.password);
    for u in &reach.urls {
        text.push_str(&format!("url = {u}\n"));
    }
    f.write_all(text.as_bytes()).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn read_reach(path: &Path) -> Option<Reach> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut r = Reach::default();
    for line in text.lines() {
        match line.split_once(" = ") {
            Some(("password", v)) => r.password = v.to_owned(),
            Some(("url", v)) => r.urls.push(v.to_owned()),
            _ => {}
        }
    }
    (!r.password.is_empty()).then_some(r)
}

/// Serves until SIGTERM.
pub fn run(
    screen: Box<dyn Screen>,
    input: Box<dyn Input>,
    only: Option<IpAddr>,
    port: u16,
    read_only: bool,
    reach_file: &Path,
) -> Result<(), String> {
    super::install_signals();
    let lans = net::lan_addresses()?;
    if lans.is_empty() {
        return Err("this laptop has no address on a private network, so nothing can reach it".into());
    }
    let password = net::new_token()?;
    let listen = lans.iter().map(|lan| Listen { addr: SocketAddr::new(IpAddr::V4(lan.ip), port), lan: *lan }).collect();
    let cfg = Config { listen, only, read_only, title: "Juno".into(), password: password.clone() };
    let server = Server::start(cfg, screen, input)?;
    let addrs: Vec<String> = server.local_addrs().iter().map(|a| a.to_string()).collect();
    let urls = server.local_addrs().iter().map(|a| url(*a, &password)).collect();
    if let Err(e) = write_reach(reach_file, &Reach { password, urls }) {
        server.stop();
        return Err(e);
    }
    eprintln!(
        "lucina-app-desk: web host: listening on {}, this network only, not encrypted{}",
        addrs.join(", "),
        if read_only { ", read-only" } else { "" }
    );
    while !super::stopping() {
        std::thread::sleep(Duration::from_millis(200));
    }
    server.stop();
    let _ = std::fs::remove_file(reach_file);
    eprintln!("lucina-app-desk: web host: stopped");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reach_file_only_root_reads_and_it_holds_the_links() {
        use std::os::unix::fs::PermissionsExt;
        let d = std::env::temp_dir().join(format!("desk-web-reach-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let f = d.join("desk-web");
        let a: SocketAddr = "192.168.1.47:6901".parse().unwrap();
        let r = Reach { password: "ab".repeat(16), urls: vec![url(a, &"ab".repeat(16))] };
        assert_eq!(r.urls[0], format!("http://192.168.1.47:6901/?token={}", "ab".repeat(16)));
        write_reach(&f, &r).unwrap();
        assert_eq!(std::fs::metadata(&f).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(read_reach(&f), Some(r));
        // Starting again leaves no old password behind.
        let r2 = Reach { password: "c".into(), urls: vec![] };
        write_reach(&f, &r2).unwrap();
        assert_eq!(read_reach(&f), Some(r2));
        let _ = std::fs::remove_dir_all(&d);
        assert_eq!(read_reach(&f), None);
    }
}
