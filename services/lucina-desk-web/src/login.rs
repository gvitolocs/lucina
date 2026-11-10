//! Who may use the host: voxl's login (password as Bearer, Basic or ?token=) and its Host/Origin
//! checks, plus a lockout. Browsers let any web page open a socket to the host or send it
//! requests, and a page whose own domain is made to resolve to the host's address (DNS
//! rebinding) even counts as the host's own page, so only a `Host` that can't be rebound (an IP
//! address or localhost) is answered, and an `Origin` from anywhere else is turned away. Every
//! other request needs the password, except the scripts and assets the page loads before it can
//! present one and the health check.

use std::time::{Duration, Instant};

/// What the page fetches before it has the password, and the health check.
const PUBLIC: &[&str] = &[
    "/client.js",
    "/worker.js",
    "/audio.js",
    "/mic.js",
    "/favicon.ico",
    "/inter.woff2",
    "/healthz",
];

/// Wrong credentials before every non-public request is locked out, and for how long.
const MAX_FAILURES: u32 = 5;
const LOCKOUT: Duration = Duration::from_secs(60);

/// What a request carries that the checks look at. Header names are matched case-insensitively by
/// the caller, which passes the values here as found (None if absent).
pub struct RequestFacts<'a> {
    pub path: &'a str,           // "/ws", without the query
    pub query: &'a str,          // "token=...&stats", without the "?"; "" if none
    pub host: Option<&'a str>,   // Host header
    pub origin: Option<&'a str>, // Origin header
    pub authorization: Option<&'a str>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    Unauthorized,
    Forbidden,
    LockedOut,
}

pub struct Login {
    password: String,
    failures: u32,
    locked_until: Option<Instant>,
}

impl Login {
    /// `password` is used as is (the caller makes it from the kernel's randomness). Never logged
    /// by this module.
    pub fn new(password: String) -> Login {
        Login {
            password,
            failures: 0,
            locked_until: None,
        }
    }

    /// The single check for every request. `now` is passed in so tests can move time.
    pub fn check(&mut self, r: &RequestFacts, now: Instant) -> Verdict {
        let Some(host) = r.host else {
            return Verdict::Forbidden;
        };
        let (name, port) = split_host(host);
        if !(name.parse::<std::net::IpAddr>().is_ok()
            || name == "localhost"
            || name.ends_with(".localhost"))
        {
            return Verdict::Forbidden;
        }
        if let Some(origin) = r.origin {
            let origin = origin.trim().to_ascii_lowercase();
            let own = split_origin(&origin).is_some_and(|(n, p, default)| {
                n == name && p.unwrap_or(default) == port.unwrap_or(default)
            });
            if !own {
                return Verdict::Forbidden;
            }
        }
        if PUBLIC.contains(&r.path) {
            return Verdict::Allow;
        }
        if let Some(deadline) = self.locked_until {
            if now < deadline {
                return Verdict::LockedOut;
            }
            self.locked_until = None;
            self.failures = 0;
        }
        match self.credential(r) {
            Some(true) => {
                self.failures = 0;
                Verdict::Allow
            }
            Some(false) => {
                self.failures += 1;
                if self.failures >= MAX_FAILURES {
                    self.locked_until = Some(now + LOCKOUT);
                    Verdict::LockedOut
                } else {
                    Verdict::Unauthorized
                }
            }
            None => Verdict::Unauthorized,
        }
    }

    /// Whether the request presents the password, as Bearer, Basic or a `token` query parameter;
    /// `None` when it presents no credential at all.
    fn credential(&self, r: &RequestFacts) -> Option<bool> {
        if let Some(authorization) = r.authorization {
            if let Some((scheme, credentials)) = authorization.trim().split_once(' ') {
                let credentials = credentials.trim();
                if scheme.eq_ignore_ascii_case("bearer") {
                    return Some(same(credentials, &self.password));
                }
                if scheme.eq_ignore_ascii_case("basic") {
                    return Some(basic_ok(credentials, &self.password));
                }
            }
        }
        query_param(r.query, "token").map(|t| same(&percent_decode(t), &self.password))
    }
}

/// Compares in time that depends only on the lengths, not on where the bytes differ.
pub fn same(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0, |diff, (x, y)| diff | (x ^ y))
            == 0
}

/// Basic credentials (`user:password` in base64) with the password and any user name.
fn basic_ok(encoded: &str, password: &str) -> bool {
    let Some(decoded) = base64_decode(encoded)
        .and_then(|b| String::from_utf8(b).ok())
    else {
        return false;
    };
    let Some((_, password_part)) = decoded.split_once(':') else {
        return false;
    };
    same(password_part, password)
}

/// The value of a query parameter, still encoded. The first occurrence wins.
fn query_param<'a>(query: &'a str, key: &str) -> Option<&'a str> {
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        (percent_decode(k) == key).then_some(v)
    })
}

/// Percent-decodes as a form decoder does: `%XX` for a byte, `+` for a space.
fn percent_decode(s: &str) -> String {
    fn hex(c: u8) -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        }
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => match (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                (Some(h), Some(l)) => {
                    out.push(h * 16 + l);
                    i += 3;
                }
                _ => {
                    out.push(b'%');
                    i += 1;
                }
            },
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Standard base64 (the `+`/`/` alphabet, padding optional at the end).
fn base64_decode(s: &str) -> Option<Vec<u8>> {
    fn value(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a' + 26) as u32),
            b'0'..=b'9' => Some((c - b'0' + 52) as u32),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let s = s.trim_end_matches('=');
    if s.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    for chunk in s.as_bytes().chunks(4) {
        let mut acc: u32 = 0;
        for &c in chunk {
            acc = (acc << 6) | value(c)?;
        }
        match chunk.len() {
            4 => {
                out.push((acc >> 16) as u8);
                out.push((acc >> 8) as u8);
                out.push(acc as u8);
            }
            3 => {
                out.push((acc >> 10) as u8);
                out.push((acc >> 2) as u8);
            }
            2 => out.push((acc >> 4) as u8),
            _ => return None,
        }
    }
    Some(out)
}

/// Splits a `Host` value into its lowercase name (an IPv6 address without the brackets) and its
/// port. A port that isn't a number becomes 0, which matches no origin.
fn split_host(v: &str) -> (String, Option<u16>) {
    let v = v.trim();
    let (name, port) = match v.strip_prefix('[') {
        Some(rest) => match rest.split_once(']') {
            Some((ip, after)) => (ip, after.strip_prefix(':')),
            None => (rest, None),
        },
        None => match v.rsplit_once(':') {
            Some((n, p)) if !n.contains(':') => (n, Some(p)),
            _ => (v, None),
        },
    };
    (
        name.trim_end_matches('.').to_ascii_lowercase(),
        port.map(|p| p.parse().unwrap_or(0)),
    )
}

/// Splits an origin into its host name, its port and its scheme's default port; `None` for one
/// that isn't http(s), such as the "null" of a sandboxed frame.
fn split_origin(v: &str) -> Option<(String, Option<u16>, u16)> {
    let (scheme, rest) = v.split_once("://")?;
    let default = match scheme {
        "http" | "ws" => 80,
        "https" | "wss" => 443,
        _ => return None,
    };
    let (name, port) = split_host(rest);
    Some((name, port, default))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PW: &str = "0123456789abcdef0123456789abcdef";

    /// A request to `path`?`query` from a browser on the host itself, with the given headers.
    fn req<'a>(
        path: &'a str,
        query: &'a str,
        host: Option<&'a str>,
        origin: Option<&'a str>,
        authorization: Option<&'a str>,
    ) -> RequestFacts<'a> {
        RequestFacts {
            path,
            query,
            host: host.or(Some("127.0.0.1:6901")),
            origin,
            authorization,
        }
    }

    fn basic(user_password: &str) -> String {
        const ALPHABET: &[u8] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let bytes = user_password.as_bytes();
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let mut acc: u32 = 0;
            for (i, &c) in chunk.iter().enumerate() {
                acc |= (c as u32) << (16 - 8 * i);
            }
            out.push(ALPHABET[(acc >> 18) as usize & 63] as char);
            out.push(ALPHABET[(acc >> 12) as usize & 63] as char);
            out.push(if chunk.len() > 1 {
                ALPHABET[(acc >> 6) as usize & 63] as char
            } else {
                '='
            });
            out.push(if chunk.len() > 2 {
                ALPHABET[acc as usize & 63] as char
            } else {
                '='
            });
        }
        format!("Basic {out}")
    }

    #[test]
    fn requests_without_the_password_are_refused() {
        let mut login = Login::new(PW.to_string());
        let now = Instant::now();
        let wrong = &PW[1..];
        let bearer_wrong = format!("Bearer {wrong}");
        for path in [
            "/",
            "/index.html",
            "/ws",
            "/screenshot",
            "/files/list",
            "/wait",
            "/stats",
        ] {
            let token_wrong = format!("token={wrong}");
            // A fresh login each time: wrong credentials count towards a lockout.
            let mut login = Login::new(PW.to_string());
            assert_eq!(
                login.check(&req(path, "path=", None, None, None), now),
                Verdict::Unauthorized,
                "{path}"
            );
            assert_eq!(
                login.check(&req(path, "", None, None, Some(&bearer_wrong)), now),
                Verdict::Unauthorized,
                "{path}"
            );
            assert_eq!(
                login.check(&req(path, &token_wrong, None, None, None), now),
                Verdict::Unauthorized,
                "{path}"
            );
        }
        // What the page loads before it can present the password, and health checks.
        for path in [
            "/client.js",
            "/worker.js",
            "/audio.js",
            "/mic.js",
            "/favicon.ico",
            "/inter.woff2",
            "/healthz",
        ] {
            assert_eq!(
                login.check(&req(path, "token=x", None, None, None), now),
                Verdict::Allow,
                "{path}"
            );
        }
    }

    #[test]
    fn each_way_of_sending_the_password_is_accepted() {
        let mut login = Login::new(PW.to_string());
        let now = Instant::now();
        let bearer = format!("Bearer {PW}");
        let bearer_lower = format!("bearer {PW}");
        let basic_empty_user = basic(&format!(":{PW}"));
        let basic_me = basic(&format!("me:{PW}"));
        let basic_wrong = basic(&format!("me:{}", &PW[1..]));
        assert_eq!(
            login.check(&req("/screenshot", "", None, None, Some(&bearer)), now),
            Verdict::Allow
        );
        assert_eq!(
            login.check(&req("/screenshot", "", None, None, Some(&bearer_lower)), now),
            Verdict::Allow
        );
        // Basic, with any user name (curl -u :password sends an empty one).
        assert_eq!(
            login.check(&req("/", "", None, None, Some(&basic_empty_user)), now),
            Verdict::Allow
        );
        assert_eq!(
            login.check(&req("/", "", None, None, Some(&basic_me)), now),
            Verdict::Allow
        );
        assert_eq!(
            login.check(&req("/", "", None, None, Some(&basic_wrong)), now),
            Verdict::Unauthorized
        );
        assert_eq!(
            login.check(&req("/", "", None, None, Some("Basic not-base64!")), now),
            Verdict::Unauthorized
        );
        // The link's query, decoded (here every byte is percent-encoded), beside the page's
        // other parameters.
        let encoded: String = PW.bytes().map(|b| format!("%{b:02X}")).collect();
        let with_encoded = format!("stats&token={encoded}");
        let plain = format!("token={PW}");
        let wrong = format!("token={}", &PW[1..]);
        assert_eq!(
            login.check(&req("/", &with_encoded, None, None, None), now),
            Verdict::Allow
        );
        assert_eq!(
            login.check(&req("/ws", &plain, None, None, None), now),
            Verdict::Allow
        );
        assert_eq!(
            login.check(&req("/ws", &wrong, None, None, None), now),
            Verdict::Unauthorized
        );
        // `+` in the query is a space, as a form decoder reads it.
        let plus = format!("token={}", PW.replace('a', "+"));
        assert_eq!(
            login.check(&req("/ws", &plus, None, None, None), now),
            Verdict::Unauthorized
        );
    }

    #[test]
    fn unlisted_host_names_are_refused() {
        let mut login = Login::new(PW.to_string());
        let now = Instant::now();
        // A host name the host wasn't told to serve.
        assert_eq!(
            login.check(&req("/ws", "", Some("other.example:6901"), None, None), now),
            Verdict::Forbidden
        );
        assert_eq!(
            login.check(
                &req(
                    "/ws",
                    "",
                    Some("other.example:6901"),
                    Some("http://other.example:6901"),
                    None
                ),
                now
            ),
            Verdict::Forbidden
        );
        // A missing Host.
        let no_host = RequestFacts {
            path: "/ws",
            query: "",
            host: None,
            origin: None,
            authorization: None,
        };
        assert_eq!(login.check(&no_host, now), Verdict::Forbidden);
        // Names that can't be rebound.
        for host in [
            "127.0.0.1:6901",
            "[::1]:6901",
            "10.0.0.5",
            "localhost:6901",
            "voxl.localhost",
        ] {
            assert_eq!(
                login.check(&req("/ws", "", Some(host), None, None), now),
                Verdict::Unauthorized,
                "{host}"
            );
        }
    }

    #[test]
    fn other_sites_pages_are_refused() {
        let mut login = Login::new(PW.to_string());
        let now = Instant::now();
        assert_eq!(
            login.check(
                &req("/ws", "", None, Some("https://another-site.example"), None),
                now
            ),
            Verdict::Forbidden
        );
        assert_eq!(
            login.check(&req("/ws", "", None, Some("null"), None), now),
            Verdict::Forbidden
        );
        // Another app on the same machine is another site.
        assert_eq!(
            login.check(
                &req("/ws", "", Some("localhost:6901"), Some("http://localhost:3000"), None),
                now
            ),
            Verdict::Forbidden
        );
        // The host's own page, and programs that send no Origin.
        assert_eq!(
            login.check(
                &req("/ws", "", None, Some("http://127.0.0.1:6901"), None),
                now
            ),
            Verdict::Unauthorized
        );
        assert_eq!(
            login.check(
                &req("/ws", "", Some("[::1]:6901"), Some("http://[::1]:6901"), None),
                now
            ),
            Verdict::Unauthorized
        );
        assert_eq!(
            login.check(&req("/ws", "", Some("[::1]:6901"), None, None), now),
            Verdict::Unauthorized
        );
        // An explicit default port equals none.
        assert_eq!(
            login.check(
                &req("/ws", "", Some("127.0.0.1"), Some("http://127.0.0.1:80"), None),
                now
            ),
            Verdict::Unauthorized
        );
    }

    #[test]
    fn lockout_after_five_wrong_credentials() {
        let mut login = Login::new(PW.to_string());
        let t0 = Instant::now();
        let wrong = format!("Bearer {}", &PW[1..]);
        for i in 0..4 {
            assert_eq!(
                login.check(&req("/ws", "", None, None, Some(&wrong)), t0),
                Verdict::Unauthorized,
                "wrong {i}"
            );
        }
        assert_eq!(
            login.check(&req("/ws", "", None, None, Some(&wrong)), t0),
            Verdict::LockedOut,
            "fifth wrong"
        );
        // Even the right password is locked out inside 60 s.
        let right = format!("Bearer {PW}");
        assert_eq!(
            login.check(&req("/ws", "", None, None, Some(&right)), t0 + Duration::from_secs(59)),
            Verdict::LockedOut
        );
        // After 60 s the count has reset and the right password works.
        assert_eq!(
            login.check(&req("/ws", "", None, None, Some(&right)), t0 + Duration::from_secs(60)),
            Verdict::Allow
        );
    }

    #[test]
    fn requests_without_a_credential_never_lock_out() {
        let mut login = Login::new(PW.to_string());
        let t0 = Instant::now();
        for i in 0..20 {
            assert_eq!(
                login.check(&req("/ws", "", None, None, None), t0),
                Verdict::Unauthorized,
                "no credential {i}"
            );
        }
        let right = format!("Bearer {PW}");
        assert_eq!(
            login.check(&req("/ws", "", None, None, Some(&right)), t0),
            Verdict::Allow
        );
    }

    #[test]
    fn public_paths_allow_while_locked_out() {
        let mut login = Login::new(PW.to_string());
        let t0 = Instant::now();
        let wrong = format!("Bearer {}", &PW[1..]);
        for _ in 0..5 {
            login.check(&req("/ws", "", None, None, Some(&wrong)), t0);
        }
        for path in ["/client.js", "/healthz"] {
            assert_eq!(
                login.check(&req(path, "", None, None, None), t0),
                Verdict::Allow,
                "{path}"
            );
        }
        // Once the lockout has ended, a successful login resets the count, so one wrong
        // credential afterwards doesn't lock out again.
        let right = format!("Bearer {PW}");
        let after = t0 + Duration::from_secs(60);
        assert_eq!(
            login.check(&req("/ws", "", None, None, Some(&right)), after),
            Verdict::Allow
        );
        assert_eq!(
            login.check(&req("/ws", "", None, None, Some(&wrong)), after),
            Verdict::Unauthorized
        );
    }
}
