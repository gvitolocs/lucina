//! lucina-resolv: host names to addresses for programs with no C library (Eyra's `getaddrinfo`,
//! `third_party/eyra-stack/c-gull`), as musl does it. Upstream c-gull ran glibc's `getent` instead, which Juno does not have.
//!
//! The original is musl 1.2.5 (`src/network/lookup_name.c`, `res_msend.c`, `resolvconf.c`, `dns_parse.c`, `lookup_ipliteral.c`):
//! - a numeric address is itself;
//! - then `/etc/hosts`: the first line's address for every line that names the host (exact, case-sensitive, as musl's `strstr`);
//! - then DNS: `/etc/resolv.conf`'s first three `nameserver`s (127.0.0.1 if none), `options ndots: timeout: attempts:`
//!   (defaults 1, 5 s, 2; at most 15, 60, 10), and `search`/`domain`. A name with fewer dots than `ndots` and no final dot is
//!   tried with each search domain first, then as it is. A and AAAA go to every server at once, sent again every
//!   `timeout / attempts`, until each has a reply or `timeout` passes; SERVFAIL asks that server again (at most twice per
//!   server), other errors from a server are ignored while others may still answer.
//! - Results: no reply or SERVFAIL for a query is `Again`; NXDOMAIN or no address, `NoName` (after the whole search list);
//!   another error, `Fail`. Addresses that cannot be reached from here (no route) sort last, then IPv6 before IPv4 (RFC 3484's
//!   default precedence, which musl's sort applies).
//!
//! The message rules are in [`wire`], proved by Verus (`verification/juno-resolv`).

pub mod wire;

use std::io::Read;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

/// Which addresses are wanted (`ai_family`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    Any,
    V4,
    V6,
}

/// Why there is no address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The name does not exist, or has no address of the family asked for (`EAI_NONAME`).
    NoName,
    /// No server answered in time, or one answered SERVFAIL: asking again later may work (`EAI_AGAIN`).
    Again,
    /// A server refused or the name cannot be asked (`EAI_FAIL`).
    Fail,
}

/// What `/etc/resolv.conf` says, with musl's defaults and limits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conf {
    pub nameservers: Vec<IpAddr>,
    pub ndots: u32,
    pub timeout_s: u32,
    pub attempts: u32,
    pub search: String,
}

impl Conf {
    /// Reads `resolv.conf`'s text as musl's `__get_resolv_conf` does.
    pub fn parse(text: &str) -> Conf {
        let mut c = Conf { nameservers: Vec::new(), ndots: 1, timeout_s: 5, attempts: 2, search: String::new() };
        for line in text.lines() {
            if let Some(rest) = keyword(line, "options") {
                for opt in rest.split_ascii_whitespace() {
                    if let Some(v) = opt.strip_prefix("ndots:") {
                        c.ndots = leading_number(v).min(15);
                    } else if let Some(v) = opt.strip_prefix("attempts:") {
                        c.attempts = leading_number(v).min(10);
                    } else if let Some(v) = opt.strip_prefix("timeout:") {
                        c.timeout_s = leading_number(v).min(60);
                    }
                }
            } else if let Some(rest) = keyword(line, "nameserver") {
                if c.nameservers.len() < 3 {
                    let word = rest.split_ascii_whitespace().next().unwrap_or("");
                    // A scope (`%eth0`) is dropped: Juno's servers are on its one network.
                    let word = word.split('%').next().unwrap_or("");
                    if let Ok(a) = word.parse::<IpAddr>() {
                        c.nameservers.push(a);
                    }
                }
            } else if let Some(rest) = keyword(line, "domain").or_else(|| keyword(line, "search")) {
                c.search = rest.trim().to_string();
            }
        }
        if c.nameservers.is_empty() {
            c.nameservers.push(IpAddr::V4(Ipv4Addr::LOCALHOST));
        }
        c
    }
}

/// `line` starts with `word` and a space or tab: the rest after it.
fn keyword<'a>(line: &'a str, word: &str) -> Option<&'a str> {
    let rest = line.strip_prefix(word)?;
    if rest.starts_with([' ', '\t']) { Some(rest) } else { None }
}

/// The digits at the start of `s`, as `strtoul` reads them (0 if none).
fn leading_number(s: &str) -> u32 {
    let digits: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse::<u64>().map(|v| v.min(u32::MAX as u64) as u32).unwrap_or(0)
}

/// The addresses `/etc/hosts`' text gives `name`, in file order, as musl's `name_from_hosts` reads it.
pub fn from_hosts(text: &str, name: &str, family: Family) -> Vec<IpAddr> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("");
        let mut words = line.split_ascii_whitespace();
        let Some(addr) = words.next() else { continue };
        if !words.any(|w| w == name) {
            continue;
        }
        let addr = addr.split('%').next().unwrap_or("");
        if let Ok(a) = addr.parse::<IpAddr>() {
            if wanted(a, family) && out.len() < 48 {
                out.push(a);
            }
        }
    }
    out
}

fn wanted(a: IpAddr, family: Family) -> bool {
    matches!((a, family), (_, Family::Any) | (IpAddr::V4(_), Family::V4) | (IpAddr::V6(_), Family::V6))
}

/// The addresses of `name`, from `/etc/hosts` then DNS with `/etc/resolv.conf`. Numeric names are themselves.
pub fn lookup(name: &str, family: Family) -> Result<Vec<IpAddr>, Error> {
    if let Ok(a) = name.parse::<IpAddr>() {
        return if wanted(a, family) { Ok(vec![a]) } else { Err(Error::NoName) };
    }
    let hosts = read_small("/etc/hosts");
    let found = from_hosts(&hosts, name, family);
    if !found.is_empty() {
        return Ok(found);
    }
    let conf = Conf::parse(&read_small("/etc/resolv.conf"));
    let mut found = search(name, family, &conf, &mut UdpTransport { port: 53 })?;
    sort_routable(&mut found);
    Ok(found)
}

/// A small text file, or "" when it cannot be read. `/etc/resolv.conf` on Juno is `/proc/net/pnp`, whose size reads as 0, so
/// this reads until the end rather than by size.
fn read_small(path: &str) -> String {
    let mut s = String::new();
    if let Ok(f) = std::fs::File::open(path) {
        let _ = f.take(64 * 1024).read_to_string(&mut s);
    }
    s
}

/// How queries reach the servers: UDP for real, a script in tests.
pub trait Transport {
    /// Sends every query to every server and returns, per query, the reply that matched it (`None` if none in time).
    fn exchange(&mut self, queries: &[Vec<u8>], conf: &Conf) -> Vec<Option<Vec<u8>>>;
}

/// musl's `name_from_dns_search`: the search domains first when the name has fewer dots than `ndots`, then the name itself.
pub fn search(name: &str, family: Family, conf: &Conf, t: &mut dyn Transport) -> Result<Vec<IpAddr>, Error> {
    let dots = name.bytes().filter(|&b| b == b'.').count() as u32;
    let final_dot = name.ends_with('.');
    let base = if final_dot { &name[..name.len() - 1] } else { name };
    if base.is_empty() || base.ends_with('.') {
        return Err(Error::NoName);
    }
    if dots < conf.ndots && !final_dot {
        for domain in conf.search.split_ascii_whitespace() {
            if base.len() + 1 + domain.len() >= 256 {
                continue;
            }
            let full = format!("{base}.{domain}");
            match from_dns(&full, family, conf, t) {
                Ok(a) if !a.is_empty() => return Ok(a),
                Ok(_) => {}
                Err(e) => return Err(e),
            }
        }
    }
    match from_dns(base, family, conf, t) {
        Ok(a) if !a.is_empty() => Ok(a),
        Ok(_) => Err(Error::NoName),
        Err(e) => Err(e),
    }
}

/// musl's `name_from_dns`: A and AAAA (as the family asks), then the answers' addresses. `Ok` of none: NXDOMAIN or no record.
fn from_dns(name: &str, family: Family, conf: &Conf, t: &mut dyn Transport) -> Result<Vec<IpAddr>, Error> {
    let types: &[u16] = match family {
        Family::Any => &[wire::TYPE_A, wire::TYPE_AAAA],
        Family::V4 => &[wire::TYPE_A],
        Family::V6 => &[wire::TYPE_AAAA],
    };
    let mut queries = Vec::new();
    let id0 = random_u16();
    for (i, &ty) in types.iter().enumerate() {
        let mut q = Vec::new();
        if !wire::query(id0.wrapping_add(i as u16), name.as_bytes(), ty, &mut q) {
            return Err(Error::NoName);
        }
        queries.push(q);
    }
    let replies = t.exchange(&queries, conf);
    // As musl: every query must have its reply, and a SERVFAIL left at the end counts as no reply.
    let mut all = Vec::new();
    let mut parsed = Vec::new();
    for (q, r) in queries.iter().zip(replies.iter()) {
        let Some(r) = r else { return Err(Error::Again) };
        let (ans, addrs) = wire::answers(r, q.len(), u16::from_be_bytes([q[q.len() - 4], q[q.len() - 3]]));
        match ans {
            wire::Answer::Retry => return Err(Error::Again),
            wire::Answer::NoName => return Ok(Vec::new()),
            wire::Answer::Failed => return Err(Error::Fail),
            wire::Answer::Found => parsed.push(addrs),
        }
    }
    for addrs in parsed {
        for a in addrs {
            all.push(match a.len {
                4 => IpAddr::V4(Ipv4Addr::from(a.value as u32)),
                _ => IpAddr::V6(Ipv6Addr::from(a.value)),
            });
        }
    }
    Ok(all)
}

/// Query ids from the kernel's generator; musl mixes the clock and an address, which is weaker.
fn random_u16() -> u16 {
    let mut b = [0u8; 2];
    if std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut b)).is_err() {
        let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0) ^ std::process::id();
        b = (n as u16).to_be_bytes();
    }
    u16::from_be_bytes(b)
}

/// Routable addresses first, then by RFC 3484's default precedence (IPv6 before IPv4); otherwise the servers' order.
fn sort_routable(addrs: &mut [IpAddr]) {
    let key = |a: &IpAddr| {
        let unroutable = {
            let local: SocketAddr = match a {
                IpAddr::V4(_) => (Ipv4Addr::UNSPECIFIED, 0).into(),
                IpAddr::V6(_) => (Ipv6Addr::UNSPECIFIED, 0).into(),
            };
            // A UDP connect sends nothing; it only asks the kernel for a route.
            UdpSocket::bind(local).and_then(|s| s.connect((*a, 65535))).is_err()
        };
        let precedence = match a {
            IpAddr::V6(v6) if v6.is_loopback() => 50,
            IpAddr::V6(_) => 40,
            IpAddr::V4(_) => 35,
        };
        (unroutable, std::cmp::Reverse(precedence))
    };
    let mut keyed: Vec<_> = addrs.iter().map(|a| (key(a), *a)).collect();
    keyed.sort_by_key(|(k, _)| *k);
    for (slot, (_, a)) in addrs.iter_mut().zip(keyed) {
        *slot = a;
    }
}

/// The real thing: one UDP socket per address family, musl's `__res_msend` schedule.
struct UdpTransport {
    port: u16,
}

impl Transport for UdpTransport {
    fn exchange(&mut self, queries: &[Vec<u8>], conf: &Conf) -> Vec<Option<Vec<u8>>> {
        let mut replies: Vec<Option<Vec<u8>>> = vec![None; queries.len()];
        let v4 = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok();
        let v6 = if conf.nameservers.iter().any(IpAddr::is_ipv6) { UdpSocket::bind((Ipv6Addr::UNSPECIFIED, 0)).ok() } else { None };
        let servers: Vec<SocketAddr> = conf.nameservers.iter().map(|a| SocketAddr::new(*a, self.port)).collect();
        let socket_for = |s: &SocketAddr| if s.is_ipv4() { v4.as_ref() } else { v6.as_ref() };
        let send_all = |only: Option<usize>, replies: &[Option<Vec<u8>>]| {
            for (i, q) in queries.iter().enumerate() {
                if replies[i].is_some() || only.is_some_and(|o| o != i) {
                    continue;
                }
                for s in &servers {
                    if let Some(sock) = socket_for(s) {
                        let _ = sock.send_to(q, s);
                    }
                }
            }
        };
        let total = Duration::from_secs(conf.timeout_s.max(1) as u64);
        let retry = total / conf.attempts.max(1);
        let start = Instant::now();
        let mut next_send = start;
        let mut servfail_left = 2 * servers.len();
        let mut buf = vec![0u8; 4096];
        while replies.iter().any(Option::is_none) {
            let now = Instant::now();
            if now >= start + total {
                break;
            }
            if now >= next_send {
                send_all(None, &replies);
                next_send = now + retry;
            }
            let wait = (next_send.min(start + total)).saturating_duration_since(Instant::now()).max(Duration::from_millis(1));
            // Two sockets at most: wait on each in turn for a slice of the time.
            let socks: Vec<&UdpSocket> = [v4.as_ref(), v6.as_ref()].into_iter().flatten().collect();
            let slice = (wait / socks.len().max(1) as u32).max(Duration::from_millis(1));
            for sock in socks {
                let _ = sock.set_read_timeout(Some(slice));
                let Ok((n, from)) = sock.recv_from(&mut buf) else { continue };
                // Only the servers asked may answer.
                if !servers.iter().any(|s| s.ip() == from.ip() && s.port() == from.port()) {
                    continue;
                }
                let r = &buf[..n];
                for (i, q) in queries.iter().enumerate() {
                    if replies[i].is_some() || !wire::matches(r, q) {
                        continue;
                    }
                    match r[3] & 0x0f {
                        0 | 3 => replies[i] = Some(r.to_vec()),
                        // SERVFAIL: ask that server again, at most twice per server in all; the reply itself is never kept, so a
                        // query whose servers only fail ends without a reply (Again), as in musl.
                        2 => {
                            if servfail_left > 0 {
                                servfail_left -= 1;
                                if let Some(sock) = socket_for(&from) {
                                    let _ = sock.send_to(q, from);
                                }
                            }
                        }
                        // Another error from one server: wait for the others.
                        _ => {}
                    }
                }
            }
        }
        replies
    }
}

/// [`search`] over real UDP to the servers of `conf` on `port` instead of 53: for tests against a local server.
#[doc(hidden)]
pub fn search_udp(name: &str, family: Family, conf: &Conf, port: u16) -> Result<Vec<IpAddr>, Error> {
    search(name, family, conf, &mut UdpTransport { port })
}
