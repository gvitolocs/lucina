//! The handshake against the real gateway, from the host: the same code the DSi runs, over a std socket.
//! Returns early (passes) when the host has no network, so offline builds still test; never `#[ignore]`.

use std::io::{Read as _, Write as _};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use embedded_io_adapters::std::FromStd;
use fortuna_tls::rng::{Pool, Rng};
use fortuna_tls::verify::Refusal;

const HOST: &str = "getsecretary.tech";

fn socket() -> Option<TcpStream> {
    let addr = (HOST, 443).to_socket_addrs().ok()?.next()?;
    let s = TcpStream::connect_timeout(&addr, Duration::from_secs(5)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(15))).ok()?;
    Some(s)
}

fn rng() -> Rng {
    let mut p = Pool::new();
    let mut seed = [0u8; 32];
    std::fs::File::open("/dev/urandom").unwrap().read_exact(&mut seed).unwrap();
    p.mix(&seed);
    Rng::from_pool(p)
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

#[test]
fn health_over_tls_1_3() {
    let Some(sock) = socket() else { return };
    let mut r = rng();
    let (mut rb, mut wb) = (vec![0u8; fortuna_tls::READ_BUF], vec![0u8; fortuna_tls::WRITE_BUF]);
    let started = std::time::Instant::now();
    let mut tls = fortuna_tls::connect(FromStd::new(sock), HOST, now(), 0, &mut r, &mut rb, &mut wb).expect("handshake");
    eprintln!("handshake {:?}", started.elapsed());
    let req = format!("GET /v1/health HTTP/1.1\r\nHost: {HOST}\r\nUser-Agent: Fortuna-test\r\nConnection: close\r\n\r\n");
    embedded_io::Write::write_all(&mut tls, req.as_bytes()).unwrap();
    embedded_io::Write::flush(&mut tls).unwrap();
    let mut got = Vec::new();
    let mut buf = [0u8; 2048];
    loop {
        match embedded_io::Read::read(&mut tls, &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => got.extend_from_slice(&buf[..n]),
        }
        if got.windows(4).any(|w| w == b"\r\n\r\n") && got.len() > 200 {
            break;
        }
    }
    let text = String::from_utf8_lossy(&got);
    assert!(text.starts_with("HTTP/1.1 200"), "{text}");
    let _ = std::io::stderr().write_all(b"ok\n");
}

#[test]
fn a_wrong_name_is_refused_by_the_chain_check() {
    let Some(sock) = socket() else { return };
    let mut r = rng();
    let (mut rb, mut wb) = (vec![0u8; fortuna_tls::READ_BUF], vec![0u8; fortuna_tls::WRITE_BUF]);
    // A name the gateway has no certificate for: Caddy answers with another certificate or an alert; either way no
    // connection. (www.getsecretary.tech is no good here: it has a real certificate of its own.)
    match fortuna_tls::connect(FromStd::new(sock), "fortuna-test.invalid", now(), 0, &mut r, &mut rb, &mut wb) {
        Ok(_) => panic!("connected under another name"),
        Err((_, refusal)) => eprintln!("refused: {}", refusal.text()),
    }
}

#[test]
fn a_clock_after_expiry_is_refused() {
    let Some(sock) = socket() else { return };
    let mut r = rng();
    let (mut rb, mut wb) = (vec![0u8; fortuna_tls::READ_BUF], vec![0u8; fortuna_tls::WRITE_BUF]);
    let Err((_, refusal)) = fortuna_tls::connect(FromStd::new(sock), HOST, now() + 400 * 86_400, 0, &mut r, &mut rb, &mut wb) else {
        panic!("accepted an expired certificate");
    };
    assert_eq!(refusal, Refusal::Expired);
}

/// A local server that reads the ClientHello, then sends `reply` and closes.
fn fake_server(reply: Vec<u8>) -> TcpStream {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    std::thread::spawn(move || {
        if let Ok((mut s, _)) = l.accept() {
            let mut buf = [0u8; 4096];
            let _ = s.read(&mut buf);
            let _ = s.write_all(&reply);
        }
    });
    let s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s
}

#[test]
fn a_truncated_record_is_refused() {
    // a handshake record whose header promises 200 bytes, then 30 of them and the end of the connection
    let mut rec = vec![0x16, 0x03, 0x03, 0x00, 200];
    rec.extend_from_slice(&[0x02, 0x00, 0x00, 0xc4]);
    rec.extend(std::iter::repeat(0x41).take(26));
    // and one whose length runs past what any record may hold
    let huge = vec![0x16, 0x03, 0x03, 0xff, 0xff, 0x02];
    for reply in [rec, huge, vec![0x16, 0x03], Vec::new()] {
        let mut r = rng();
        let (mut rb, mut wb) = (vec![0u8; fortuna_tls::READ_BUF], vec![0u8; fortuna_tls::WRITE_BUF]);
        assert!(fortuna_tls::connect(FromStd::new(fake_server(reply)), HOST, now(), 0, &mut r, &mut rb, &mut wb).is_err());
    }
}

#[test]
fn the_gateways_first_flight_cut_anywhere_is_refused() {
    // relay to the real gateway, but pass on only the first `cut` bytes of what it answers
    let Some(_) = socket() else { return };
    for cut in [5usize, 60, 300, 1500, 3000] {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        std::thread::spawn(move || {
            let Ok((mut c, _)) = l.accept() else { return };
            let Some(mut up) = socket() else { return };
            let mut buf = [0u8; 4096];
            let n = c.read(&mut buf).unwrap_or(0);
            let _ = up.write_all(&buf[..n]);
            let mut sent = 0;
            while sent < cut {
                let Ok(k) = up.read(&mut buf) else { break };
                if k == 0 {
                    break;
                }
                let take = k.min(cut - sent);
                let _ = c.write_all(&buf[..take]);
                sent += take;
            }
        });
        let s = TcpStream::connect(addr).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let mut r = rng();
        let (mut rb, mut wb) = (vec![0u8; fortuna_tls::READ_BUF], vec![0u8; fortuna_tls::WRITE_BUF]);
        assert!(fortuna_tls::connect(FromStd::new(s), HOST, now(), 0, &mut r, &mut rb, &mut wb).is_err(), "cut at {cut}");
    }
}
