//! lucina-resolv against musl's rules: the message checks, resolv.conf and hosts, the search order, and real UDP to a local
//! server that answers like a FRITZ!Box.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, UdpSocket};
use std::sync::{Arc, Mutex};

use lucina_resolv::wire::{self, Answer};
use lucina_resolv::{Conf, Error, Family, Transport, from_hosts, search, search_udp};

/// A reply to `q` with `rcode` and the given (type, rdata) answers, each owned by a pointer to the question's name.
fn reply(q: &[u8], rcode: u8, answers: &[(u16, &[u8])]) -> Vec<u8> {
    let mut r = q.to_vec();
    r[2] |= 0x80;
    r[3] = (r[3] & 0xf0) | rcode;
    r[6] = 0;
    r[7] = answers.len() as u8;
    for (ty, data) in answers {
        r.extend_from_slice(&[0xc0, 0x0c]);
        r.extend_from_slice(&ty.to_be_bytes());
        r.extend_from_slice(&1u16.to_be_bytes());
        r.extend_from_slice(&300u32.to_be_bytes());
        r.extend_from_slice(&(data.len() as u16).to_be_bytes());
        r.extend_from_slice(data);
    }
    r
}

fn qtype(q: &[u8]) -> u16 {
    u16::from_be_bytes([q[q.len() - 4], q[q.len() - 3]])
}

fn qname(q: &[u8]) -> String {
    let mut p = 12;
    let mut parts = Vec::new();
    while q[p] != 0 {
        let l = q[p] as usize;
        parts.push(String::from_utf8_lossy(&q[p + 1..p + 1 + l]).into_owned());
        p += 1 + l;
    }
    parts.join(".")
}

#[test]
fn names_follow_the_label_rules() {
    let l63 = "a".repeat(63);
    let l64 = "a".repeat(64);
    assert!(wire::name_ok(b"pool.ntp.org"));
    assert!(wire::name_ok(l63.as_bytes()));
    assert!(!wire::name_ok(l64.as_bytes()));
    assert!(wire::name_ok(format!("{l63}.{l63}").as_bytes()));
    assert!(!wire::name_ok(format!("{l63}.{l64}").as_bytes()));
    let n253 = format!("{l63}.{l63}.{l63}.{}", "b".repeat(61));
    assert_eq!(n253.len(), 253);
    assert!(wire::name_ok(n253.as_bytes()));
    assert!(!wire::name_ok(format!("{n253}c").as_bytes()));
    for bad in ["", ".", ".a", "a.", "a..b"] {
        assert!(!wire::name_ok(bad.as_bytes()), "{bad:?}");
    }
}

#[test]
fn a_query_is_musls_bytes() {
    let mut q = Vec::new();
    assert!(wire::query(0x1234, b"pool.ntp.org", wire::TYPE_AAAA, &mut q));
    let want: &[u8] = &[
        0x12, 0x34, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0, 4, b'p', b'o', b'o', b'l', 3, b'n', b't', b'p', 3, b'o', b'r', b'g', 0, 0,
        28, 0, 1,
    ];
    assert_eq!(q, want);
    let mut bad = Vec::new();
    assert!(!wire::query(1, b"a..b", wire::TYPE_A, &mut bad));
    assert!(bad.is_empty());
}

#[test]
fn only_the_matching_reply_counts() {
    let mut q = Vec::new();
    wire::query(7, b"juno.fritz.box", wire::TYPE_A, &mut q);
    let good = reply(&q, 0, &[(1, &[192, 168, 178, 47])]);
    assert!(wire::matches(&good, &q));
    let mut other_id = good.clone();
    other_id[1] ^= 1;
    assert!(!wire::matches(&other_id, &q));
    let mut not_reply = good.clone();
    not_reply[2] &= 0x7f;
    assert!(!wire::matches(&not_reply, &q));
    // One byte of the question differs, case included (musl compares bytes).
    let mut other_name = good.clone();
    other_name[13] = b'J';
    assert!(!wire::matches(&other_name, &q));
    let mut two_questions = good.clone();
    two_questions[5] = 2;
    assert!(!wire::matches(&two_questions, &q));
    assert!(!wire::matches(&good[..q.len() - 1], &q));
}

#[test]
fn answers_take_addresses_and_pass_over_the_rest() {
    let mut q = Vec::new();
    wire::query(9, b"www.example.org", wire::TYPE_A, &mut q);
    // A CNAME first (its data a name), then two A records, one with the wrong length.
    let cname: &[u8] = &[3, b'c', b'd', b'n', 0xc0, 0x0c];
    let r = reply(&q, 0, &[(5, cname), (1, &[93, 184, 215, 14]), (1, &[1, 2, 3]), (1, &[10, 0, 0, 1])]);
    let (ans, addrs) = wire::answers(&r, q.len(), wire::TYPE_A);
    assert_eq!(ans, Answer::Found);
    let got: Vec<u32> = addrs.iter().map(|a| a.value as u32).collect();
    assert_eq!(got, vec![u32::from_be_bytes([93, 184, 215, 14]), u32::from_be_bytes([10, 0, 0, 1])]);
    assert_eq!(wire::answers(&reply(&q, 3, &[]), q.len(), 1).0, Answer::NoName);
    assert_eq!(wire::answers(&reply(&q, 2, &[]), q.len(), 1).0, Answer::Retry);
    assert_eq!(wire::answers(&reply(&q, 5, &[]), q.len(), 1).0, Answer::Failed);
    let mut cut = reply(&q, 0, &[(1, &[1, 1, 1, 1])]);
    cut[2] |= 0x02;
    assert_eq!(wire::answers(&cut, q.len(), 1).0, Answer::Failed);
    // An answer count larger than the message: what fits is read, nothing beyond.
    let mut short = reply(&q, 0, &[(1, &[1, 1, 1, 1])]);
    short[7] = 200;
    let (ans, addrs) = wire::answers(&short, q.len(), 1);
    assert_eq!((ans, addrs.len()), (Answer::Found, 1));
    // AAAA: 16 bytes.
    let mut q6 = Vec::new();
    wire::query(10, b"www.example.org", wire::TYPE_AAAA, &mut q6);
    let v6: Ipv6Addr = "2606:2800:21f:cb07:6820:80da:af6b:8b2c".parse().unwrap();
    let (_, a6) = wire::answers(&reply(&q6, 0, &[(28, &v6.octets())]), q6.len(), wire::TYPE_AAAA);
    assert_eq!(Ipv6Addr::from(a6[0].value), v6);
}

#[test]
fn names_end_where_they_end() {
    let buf = [3, b'a', b'b', b'c', 0, 0xc0, 0x00, 5, b'x'];
    assert_eq!(wire::skip_name(&buf, 0), Some(5));
    assert_eq!(wire::skip_name(&buf, 5), Some(7));
    assert_eq!(wire::skip_name(&buf, 7), None);
    assert_eq!(wire::skip_name(&[0xc0], 0), None);
    assert_eq!(wire::skip_name(&[0x40, 0], 0), None);
}

#[test]
fn resolv_conf_as_musl_reads_it() {
    let c = Conf::parse("");
    assert_eq!(c.nameservers, vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]);
    assert_eq!((c.ndots, c.timeout_s, c.attempts), (1, 5, 2));
    // What Juno's /etc/resolv.conf (/proc/net/pnp) holds after DHCP.
    let pnp = "#PROTO: DHCP\ndomain fritz.box\nnameserver 192.168.178.1\nbootserver 192.168.178.1\n";
    let c = Conf::parse(pnp);
    assert_eq!(c.nameservers, vec!["192.168.178.1".parse::<IpAddr>().unwrap()]);
    assert_eq!(c.search, "fritz.box");
    let c = Conf::parse(
        "nameserver 1.1.1.1\nnameserver fe80::1%wlan0\nnameserver 8.8.8.8\nnameserver 9.9.9.9\nnameserverx 7.7.7.7\n\
         options ndots:20 timeout:2 attempts:30\nsearch a.lan b.lan\n",
    );
    assert_eq!(c.nameservers.len(), 3);
    assert_eq!(c.nameservers[1], "fe80::1".parse::<IpAddr>().unwrap());
    assert_eq!((c.ndots, c.timeout_s, c.attempts), (15, 2, 10));
    assert_eq!(c.search, "a.lan b.lan");
}

#[test]
fn hosts_as_musl_reads_it() {
    let hosts = "127.0.0.1 localhost # loopback\n::1 localhost ip6-localhost\n# 10.0.0.1 localhost\n192.168.178.1 fritz.box Router\n";
    assert_eq!(from_hosts(hosts, "localhost", Family::Any).len(), 2);
    assert_eq!(from_hosts(hosts, "localhost", Family::V6), vec!["::1".parse::<IpAddr>().unwrap()]);
    assert_eq!(from_hosts(hosts, "Router", Family::V4).len(), 1);
    assert!(from_hosts(hosts, "router", Family::Any).is_empty());
    assert!(from_hosts(hosts, "fritz", Family::Any).is_empty());
}

/// Answers from a table of name -> (rcode, A addresses); records the names asked.
struct Script {
    table: Vec<(&'static str, u8, Vec<[u8; 4]>)>,
    asked: Vec<String>,
}

impl Transport for Script {
    fn exchange(&mut self, queries: &[Vec<u8>], _conf: &Conf) -> Vec<Option<Vec<u8>>> {
        queries
            .iter()
            .map(|q| {
                let name = qname(q);
                self.asked.push(format!("{name}/{}", qtype(q)));
                let (_, rcode, addrs) = self.table.iter().find(|(n, _, _)| *n == name)?;
                let recs: Vec<(u16, &[u8])> =
                    if qtype(q) == wire::TYPE_A { addrs.iter().map(|a| (1u16, &a[..])).collect() } else { Vec::new() };
                Some(reply(q, *rcode, &recs))
            })
            .collect()
    }
}

#[test]
fn search_order_is_musls() {
    let conf = Conf::parse("nameserver 192.168.178.1\nsearch fritz.box\n");
    let mut s = Script { table: vec![("juno.fritz.box", 0, vec![[192, 168, 178, 47]]), ("juno", 3, vec![])], asked: vec![] };
    assert_eq!(search("juno", Family::V4, &conf, &mut s).unwrap(), vec!["192.168.178.47".parse::<IpAddr>().unwrap()]);
    assert_eq!(s.asked, vec!["juno.fritz.box/1"]);
    // Enough dots: no search domain. A final dot: none either.
    let mut s = Script { table: vec![("pool.ntp.org", 0, vec![[1, 2, 3, 4]])], asked: vec![] };
    search("pool.ntp.org", Family::Any, &conf, &mut s).unwrap();
    assert_eq!(s.asked, vec!["pool.ntp.org/1", "pool.ntp.org/28"]);
    let mut s = Script { table: vec![("juno", 0, vec![[5, 6, 7, 8]])], asked: vec![] };
    search("juno.", Family::V4, &conf, &mut s).unwrap();
    assert_eq!(s.asked, vec!["juno/1"]);
    // Not found under the search domain: the name as it is, then NoName.
    let mut s = Script { table: vec![("x.fritz.box", 3, vec![]), ("x", 3, vec![])], asked: vec![] };
    assert_eq!(search("x", Family::V4, &conf, &mut s), Err(Error::NoName));
    assert_eq!(s.asked, vec!["x.fritz.box/1", "x/1"]);
    // No reply: Again, and the search stops there.
    let mut s = Script { table: vec![], asked: vec![] };
    assert_eq!(search("x", Family::V4, &conf, &mut s), Err(Error::Again));
    assert_eq!(s.asked.len(), 1);
    // A refusal: Fail.
    let mut s = Script { table: vec![("y", 5, vec![])], asked: vec![] };
    assert_eq!(search("y.", Family::V4, &conf, &mut s), Err(Error::Fail));
    assert_eq!(search("a..", Family::V4, &conf, &mut s), Err(Error::NoName));
}

/// A server on 127.0.0.1 that answers A for every name, after sending one stray reply and one SERVFAIL first.
#[test]
fn real_udp_with_a_stray_reply_and_a_servfail() {
    let server = UdpSocket::bind("127.0.0.1:0").unwrap();
    let port = server.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(0));
    let seen2 = seen.clone();
    let t = std::thread::spawn(move || {
        let mut buf = [0u8; 512];
        let mut servfail_sent = false;
        loop {
            let Ok((n, from)) = server.recv_from(&mut buf) else { return };
            let q = buf[..n].to_vec();
            *seen2.lock().unwrap() += 1;
            // A stray reply first: wrong id. It must be dropped.
            let mut stray = reply(&q, 0, &[(1, &[6, 6, 6, 6])]);
            stray[1] ^= 0xff;
            server.send_to(&stray, from).unwrap();
            if qtype(&q) == wire::TYPE_A && !servfail_sent {
                servfail_sent = true;
                server.send_to(&reply(&q, 2, &[]), from).unwrap();
                continue;
            }
            let recs: Vec<(u16, &[u8])> = if qtype(&q) == wire::TYPE_A { vec![(1, &[192, 168, 178, 47])] } else { vec![] };
            server.send_to(&reply(&q, 0, &recs), from).unwrap();
            if *seen2.lock().unwrap() >= 3 {
                return;
            }
        }
    });
    let conf = Conf::parse("nameserver 127.0.0.1\noptions timeout:3 attempts:1\n");
    let got = search_udp("juno.lan", Family::Any, &conf, port).unwrap();
    assert_eq!(got, vec!["192.168.178.47".parse::<IpAddr>().unwrap()]);
    t.join().unwrap();
    // A, AAAA, and A again after the SERVFAIL.
    assert_eq!(*seen.lock().unwrap(), 3);
}

#[test]
fn real_udp_timeout_is_again() {
    // A socket that never answers.
    let quiet = UdpSocket::bind("127.0.0.1:0").unwrap();
    let port = quiet.local_addr().unwrap().port();
    let conf = Conf::parse("nameserver 127.0.0.1\noptions timeout:1 attempts:2\n");
    let start = std::time::Instant::now();
    assert_eq!(search_udp("nobody.lan", Family::V4, &conf, port), Err(Error::Again));
    let took = start.elapsed();
    assert!(took >= std::time::Duration::from_millis(950) && took < std::time::Duration::from_secs(3), "{took:?}");
    // Sent twice: attempts:2 in a 1 s timeout.
    let mut buf = [0u8; 512];
    quiet.set_nonblocking(true).unwrap();
    let mut n = 0;
    while quiet.recv_from(&mut buf).is_ok() {
        n += 1;
    }
    assert_eq!(n, 2);
}
