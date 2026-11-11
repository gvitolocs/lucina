//! The host a client dials: `--host --direct`. A stock RustDesk client takes an IP address and needs no server, so it works from any
//! machine on this network without touching that machine's settings. What that costs, as in stock RustDesk: **the picture and the
//! input cross the network unencrypted**. So this listens only on private addresses and takes connections only from the network of
//! the address they reached, with a password made fresh from the kernel's randomness each time it starts. It is never written to the
//! log; it stays in a file only root reads, in RAM, while the host runs, and the app shows it on this laptop's own screen.

use std::io::Write;
use std::net::{IpAddr, TcpListener, TcpStream};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use lucinadesk_core::host::{HostSession, PasswordRecord};
use lucinadesk_core::net;

use super::capture::Source;

/// What a client dials when it is given only an address.
pub const PORT: u16 = 21118;
pub const REACH_FILE: &str = "/run/lucina/desk-reach";
/// A client that stops reading gets dropped, rather than holding the only session.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
/// No lookalikes: this gets read off a screen and typed on another machine.
const ALPHABET: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";

/// The rule for who may connect is shared with the browser host (`--host --web`): lucina-desk-web's `net`.
pub use lucina_desk_web::net::{Lan, allowed, lan_addresses};

/// 80 bits from the kernel, in four groups of four.
pub fn new_password() -> Result<String, String> {
    let mut raw = [0u8; 16];
    lucina_desk_web::net::random_bytes(&mut raw)?;
    Ok(format_password(&raw))
}

fn format_password(raw: &[u8; 16]) -> String {
    let mut s = String::with_capacity(19);
    for (i, b) in raw.iter().enumerate() {
        if i > 0 && i % 4 == 0 {
            s.push('-');
        }
        s.push(ALPHABET[(*b & 31) as usize] as char);
    }
    s
}

/// What someone needs to connect, as the running host left it for the app.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Reach {
    pub password: String,
    pub addresses: Vec<String>,
}

fn write_reach(path: &Path, reach: &Reach) -> Result<(), String> {
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
    for a in &reach.addresses {
        text.push_str(&format!("address = {a}\n"));
    }
    f.write_all(text.as_bytes()).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn read_reach(path: &Path) -> Option<Reach> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut r = Reach::default();
    for line in text.lines() {
        match line.split_once(" = ") {
            Some(("password", v)) => r.password = v.to_owned(),
            Some(("address", v)) => r.addresses.push(v.to_owned()),
            _ => {}
        }
    }
    (!r.password.is_empty()).then_some(r)
}

/// One listener's connections, handed to the one loop that serves them.
fn listen(listener: TcpListener, lan: Lan, tx: mpsc::Sender<(TcpStream, IpAddr, Lan)>) {
    std::thread::spawn(move || {
        loop {
            match listener.accept() {
                Ok((s, peer)) => {
                    if tx.send((s, peer.ip(), lan)).is_err() {
                        return;
                    }
                }
                Err(e) => {
                    eprintln!("lucina-app-desk: host: accept on {}: {e}", lan.ip);
                    std::thread::sleep(Duration::from_secs(1));
                }
            }
        }
    });
}

/// Serves until SIGTERM. `only` narrows it from the whole network to one machine.
pub fn run(mut source: Box<dyn Source>, only: Option<IpAddr>, port: u16, reach_file: &Path) -> Result<(), String> {
    super::install_signals();
    let lans = lan_addresses()?;
    if lans.is_empty() {
        return Err("this laptop has no address on a private network, so nothing can reach it".into());
    }
    let (tx, rx) = mpsc::channel();
    let mut addresses = Vec::new();
    for lan in &lans {
        let listener = TcpListener::bind((lan.ip, port)).map_err(|e| format!("{}:{port}: {e}", lan.ip))?;
        addresses.push(if port == PORT { lan.ip.to_string() } else { format!("{}:{port}", lan.ip) });
        listen(listener, *lan, tx.clone());
    }
    let password = new_password()?;
    let record = PasswordRecord::new(&password);
    write_reach(reach_file, &Reach { password, addresses: addresses.clone() })?;
    eprintln!("lucina-app-desk: host: listening on {}, this network only, not encrypted", addresses.join(", "));
    let mut failures: Vec<Instant> = Vec::new();
    while !super::stopping() {
        let Ok((stream, peer, lan)) = rx.recv_timeout(Duration::from_secs(1)) else { continue };
        if !allowed(peer, lan, only) {
            eprintln!("lucina-app-desk: host: refused {peer}: not allowed here");
            continue;
        }
        failures.retain(|t| t.elapsed() < super::LOCKOUT);
        if failures.len() as u32 >= super::MAX_FAILURES {
            eprintln!("lucina-app-desk: host: {} wrong passwords: not answering for a while", super::MAX_FAILURES);
            continue;
        }
        eprintln!("lucina-app-desk: host: connection from {peer}");
        match serve_one(stream, &record, source.as_mut()) {
            Ok(wrong) => (0..wrong).for_each(|_| failures.push(Instant::now())),
            Err(e) => eprintln!("lucina-app-desk: host: {e}"),
        }
    }
    let _ = std::fs::remove_file(reach_file);
    eprintln!("lucina-app-desk: host: stopped");
    Ok(())
}

fn serve_one(stream: TcpStream, record: &PasswordRecord, source: &mut dyn Source) -> Result<i32, String> {
    stream.set_write_timeout(Some(WRITE_TIMEOUT)).map_err(|e| e.to_string())?;
    let (r, w) = net::accepted(stream).map_err(|e| e.to_string())?;
    let s = HostSession::direct(record.clone(), super::host_info(source));
    super::serve(s, net::Reader::Tcp(r), net::Writer::Tcp(w), source)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passwords_are_sixteen_clear_characters_and_all_the_bytes_count() {
        let p = format_password(&[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 31]);
        assert_eq!(p, "0123-4567-89ab-cdez");
        assert_eq!(p.len(), 19);
        assert!(!ALPHABET.iter().any(|c| b"ilou".contains(c)));
        let (a, b) = (new_password().unwrap(), new_password().unwrap());
        assert_ne!(a, b);
    }

    #[test]
    fn the_reach_file_only_root_reads_and_it_says_what_is_needed() {
        use std::os::unix::fs::PermissionsExt;
        let d = std::env::temp_dir().join(format!("desk-reach-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let f = d.join("reach");
        let r = Reach { password: "a-b".into(), addresses: vec!["192.168.1.47".into(), "192.168.1.63".into()] };
        write_reach(&f, &r).unwrap();
        assert_eq!(std::fs::metadata(&f).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(read_reach(&f), Some(r));
        // Starting again leaves no old password behind.
        let r2 = Reach { password: "c".into(), addresses: vec![] };
        write_reach(&f, &r2).unwrap();
        assert_eq!(read_reach(&f), Some(r2));
        let _ = std::fs::remove_dir_all(&d);
        assert_eq!(read_reach(&f), None);
    }

    #[test]
    fn a_real_connection_gets_the_login_first_and_nothing_before_it() {
        use super::super::capture::TestPattern;
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let t = std::thread::spawn(move || {
            let record = PasswordRecord::new("the password");
            let (s, _) = l.accept().unwrap();
            let mut source = TestPattern { w: 64, h: 64, frame: 0, moving: false };
            serve_one(s, &record, &mut source)
        });
        let (mut r, w) = net::accepted(TcpStream::connect(addr).unwrap()).unwrap();
        // A stock client's empty first message. The login's answers are `lucinadesk-core`'s tests.
        w.send(&[]).unwrap();
        assert!(!r.read().unwrap().unwrap().is_empty());
        // Leaving now: no frame came, and no wrong password is counted.
        w.shutdown();
        assert_eq!(r.read().unwrap(), None);
        assert_eq!(t.join().unwrap(), Ok(0));
    }
}
