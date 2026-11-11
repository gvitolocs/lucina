//! Requests to the account server, never on the UI thread.
//!
//! An app's store decides what to send; a [`Worker`] thread sends one [`Job`] at a time and hands back a [`Reply`]. The
//! store never sees how it travelled, so the tests and the demo run the same code as the real thing with another
//! [`Transport`] behind it.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::mpsc;
use std::time::Duration;

use lucina_account::{system, ApiError as ApiErrorV1, Client};
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Patch,
    Delete,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Post => "POST",
            Method::Patch => "PATCH",
            Method::Delete => "DELETE",
        }
    }
}

/// An answer of any status. `body` is `Null` when it was not JSON.
#[derive(Clone, Debug)]
pub struct Response {
    pub status: u16,
    pub body: Value,
    pub etag: Option<String>,
}

/// One way to reach the server. `Err` means no answer came: the request may or may not have arrived.
pub trait Transport: Send {
    fn call(
        &mut self,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Response, String>;
}

/// What went wrong with a request, like the web's `ApiError`. Status 0 is no answer at all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiError {
    pub status: u16,
    pub code: String,
    pub message: String,
}

impl ApiError {
    pub fn is_network(&self) -> bool {
        self.status == 0
    }

    /// `web/src/state/shopping.ts` `isTransient`: the same request may succeed later.
    pub fn is_transient(&self) -> bool {
        self.is_network() || self.status == 429 || self.status >= 500
    }
}

/// A transport's result as the body of a 2xx, or the error the server named (`{"error": {code, message}}`).
pub fn check(r: &Result<Response, String>) -> Result<&Value, ApiError> {
    match r {
        Err(m) => Err(ApiError {
            status: 0,
            code: "network".into(),
            message: m.clone(),
        }),
        Ok(a) if (200..300).contains(&a.status) => Ok(&a.body),
        Ok(a) => Err(ApiError {
            status: a.status,
            code: a.body["error"]["code"]
                .as_str()
                .unwrap_or("error")
                .to_owned(),
            message: a.body["error"]["message"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| format!("Request failed ({}).", a.status)),
        }),
    }
}

/// `encodeURIComponent`.
pub fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'!'
            | b'~'
            | b'*'
            | b'\''
            | b'('
            | b')' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[derive(Debug)]
pub struct Job {
    pub id: u64,
    pub method: Method,
    pub path: String,
    pub body: Option<Value>,
}

#[derive(Debug)]
pub struct Reply {
    pub id: u64,
    pub result: Result<Response, String>,
}

/// Sends jobs in order on its own thread. Dropping it ends the thread after the job in hand.
pub struct Worker {
    jobs: mpsc::Sender<Job>,
}

impl Worker {
    /// `transport` is `None` when this laptop is not signed in: every job then answers "not signed in".
    pub fn spawn(
        mut transport: Option<Box<dyn Transport>>,
        deliver: impl Fn(Reply) + Send + 'static,
    ) -> Worker {
        let (jobs, rx) = mpsc::channel::<Job>();
        std::thread::spawn(move || {
            while let Ok(j) = rx.recv() {
                let result = match transport.as_mut() {
                    Some(t) => t.call(j.method, &j.path, j.body.as_ref()),
                    None => Err(NOT_SIGNED_IN.to_owned()),
                };
                deliver(Reply { id: j.id, result });
            }
        });
        Worker { jobs }
    }

    pub fn send(&self, job: Job) {
        let _ = self.jobs.send(job);
    }
}

pub const NOT_SIGNED_IN: &str = "not signed in";

/// The owner's account, through `lucina-account`'s client.
pub struct Account(pub Client);

impl Transport for Account {
    fn call(
        &mut self,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Response, String> {
        let empty = Value::Object(Default::default());
        let b = body.unwrap_or(&empty);
        let r = match method {
            Method::Get => self.0.get_json(path),
            Method::Post => self.0.post_json(path, b),
            m => self.0.send_json(m.as_str(), path, b),
        };
        let fail = |status: u16, code: &str, message: String| {
            Ok(Response {
                status,
                body: serde_json::json!({"error": {"code": code, "message": message}}),
                etag: None,
            })
        };
        match r {
            Ok(body) => Ok(Response {
                status: 200,
                body,
                etag: None,
            }),
            Err(ApiErrorV1::Unauthorized(m)) => fail(401, "unauthorized", m),
            Err(ApiErrorV1::RateLimited(s)) => fail(
                429,
                "rate_limited",
                format!("Too many requests; wait {s} s"),
            ),
            Err(ApiErrorV1::Http(s, c, m)) => fail(s, &c, m),
            Err(ApiErrorV1::Network(m)) => Err(m),
            Err(ApiErrorV1::Protocol(m)) => Err(m),
        }
    }
}

/// The transport for this run: `None` when this laptop is not signed in. The machine's sign-in, the same for every app.
pub fn connect() -> Option<Box<dyn Transport>> {
    let token = system::token()?;
    Client::from_env(Some(token)).map(|c| Box::new(Account(c)) as Box<dyn Transport>)
}

/// The server refused the token: the account service checks, and signs every app out if it is dead.
pub fn forget_token() {
    system::report_rejected();
}

/// HTTP/1.1 to this machine and nothing else, with any token: a local `secretary-server` and its service token in parity
/// tests. The apps themselves go through [`Account`].
pub struct Loopback {
    addr: String,
    token: Option<String>,
}

impl Loopback {
    pub fn new(base: &str, token: Option<String>) -> Option<Loopback> {
        let addr = base.strip_prefix("http://")?.trim_end_matches('/');
        let host = addr.rsplit_once(':').map_or(addr, |(h, _)| h);
        matches!(host, "127.0.0.1" | "localhost" | "[::1]").then(|| Loopback {
            addr: addr.to_owned(),
            token,
        })
    }
}

fn dechunk(mut b: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(i) = b.windows(2).position(|w| w == b"\r\n") {
        let size = std::str::from_utf8(&b[..i])
            .ok()
            .and_then(|s| usize::from_str_radix(s.trim(), 16).ok())
            .unwrap_or(0);
        if size == 0 || b.len() < i + 2 + size {
            break;
        }
        out.extend_from_slice(&b[i + 2..i + 2 + size]);
        b = b.get(i + 2 + size + 2..).unwrap_or(&[]);
    }
    out
}

impl Transport for Loopback {
    fn call(
        &mut self,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Response, String> {
        let mut s = TcpStream::connect(&self.addr).map_err(|e| e.to_string())?;
        s.set_read_timeout(Some(Duration::from_secs(15))).ok();
        let data = body.map(|b| b.to_string()).unwrap_or_default();
        let mut head = format!(
            "{} {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nAccept: application/json\r\nX-Secretary-Client: web\r\nContent-Length: {}\r\n",
            method.as_str(),
            self.addr,
            data.len()
        );
        if body.is_some() {
            head.push_str("Content-Type: application/json\r\n");
        }
        if let Some(t) = &self.token {
            head.push_str(&format!("Authorization: Bearer {t}\r\n"));
        }
        head.push_str("\r\n");
        s.write_all(head.as_bytes()).map_err(|e| e.to_string())?;
        s.write_all(data.as_bytes()).map_err(|e| e.to_string())?;
        let mut raw = Vec::new();
        s.read_to_end(&mut raw).map_err(|e| e.to_string())?;
        let split = raw
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .ok_or("no header end")?;
        let headers = String::from_utf8_lossy(&raw[..split]).to_string();
        let status = headers
            .split_whitespace()
            .nth(1)
            .and_then(|c| c.parse().ok())
            .ok_or("no status")?;
        let header = |name: &str| {
            headers.lines().find_map(|l| {
                let (k, v) = l.split_once(':')?;
                k.eq_ignore_ascii_case(name).then(|| v.trim().to_owned())
            })
        };
        let payload = &raw[split + 4..];
        let chunked =
            header("transfer-encoding").is_some_and(|v| v.eq_ignore_ascii_case("chunked"));
        let decoded = if chunked {
            dechunk(payload)
        } else {
            payload.to_vec()
        };
        Ok(Response {
            status,
            body: serde_json::from_slice(&decoded).unwrap_or(Value::Null),
            etag: header("etag"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn check_names_the_servers_error() {
        let r = Ok(Response {
            status: 409,
            body: json!({"error": {"code": "conflict", "message": "changed"}}),
            etag: None,
        });
        let e = check(&r).unwrap_err();
        assert_eq!(
            (e.status, e.code.as_str(), e.message.as_str()),
            (409, "conflict", "changed")
        );
        assert!(!e.is_transient());
        assert!(
            check(&Err("timed out".to_owned()))
                .unwrap_err()
                .is_network()
        );
    }

    #[test]
    fn enc_is_encode_uri_component() {
        assert_eq!(enc("a b/c#ü"), "a%20b%2Fc%23%C3%BC");
    }

    #[test]
    fn loopback_refuses_other_machines() {
        assert!(Loopback::new("http://127.0.0.1:8080", None).is_some());
        assert!(Loopback::new("http://getsecretary.tech", None).is_none());
        assert!(Loopback::new("https://127.0.0.1", None).is_none());
    }

    #[test]
    fn a_worker_without_a_transport_says_not_signed_in() {
        let (tx, rx) = mpsc::channel();
        let w = Worker::spawn(None, move |r| {
            let _ = tx.send(r);
        });
        w.send(Job {
            id: 7,
            method: Method::Get,
            path: "/v1/notes".into(),
            body: None,
        });
        let r = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(r.id, 7);
        assert_eq!(r.result.unwrap_err(), NOT_SIGNED_IN);
    }

    #[test]
    fn dechunk_joins_the_chunks() {
        assert_eq!(dechunk(b"3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n"), b"abcde");
    }
}
