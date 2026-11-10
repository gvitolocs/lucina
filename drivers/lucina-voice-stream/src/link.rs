//! The blocking WebSocket to `wss://getsecretary.tech/v1/voice/ws`: tungstenite over rustls with the bundled webpki roots (Juno
//! has no CA store), one thread that owns the socket. It is opened when the voice UI shows and kept: it answers pings (the server
//! pings every 20 s and drops a socket silent for 50 s), and when the connection ends it reopens with a backoff of 1, 2, 4 ...
//! up to 30 seconds. The token goes only in the upgrade request's `Authorization` header, and is in no message, error or log line.
use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tungstenite::client::IntoClientRequest;
use tungstenite::http::HeaderValue;
use tungstenite::{HandshakeError, Message, WebSocket};

/// The longest wait between two tries.
pub const BACKOFF_MAX: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// A socket that has said nothing for this long is dead: the server pings every 20 s and closes one silent for 50 s. This is
/// the only timeout an open link has; otherwise it blocks in poll(2) and makes no wakes at all.
const DEAD_AFTER: Duration = Duration::from_secs(60);
/// 2026-01-01: before this the clock is wrong, not the certificate (Juno starts in 1970 until `lucina-timesync` has run).
const CLOCK_FLOOR_SECS: u64 = 1_767_225_600;

#[derive(Clone)]
pub struct LinkConfig {
    /// `wss://host/v1/voice/ws`, or `ws://127.0.0.1:port/...` for a local server (the tests, the demos).
    pub url: String,
    /// The device token, sent as `Authorization: Bearer`. Its `Debug` prints nothing.
    pub bearer: Option<String>,
}

impl std::fmt::Debug for LinkConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LinkConfig(..)")
    }
}

/// What the owner of the link sends.
#[derive(Debug)]
pub enum Outgoing {
    Text(String),
    Binary(Vec<u8>),
}

/// What the link reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Incoming {
    /// A socket is open and upgraded.
    Up,
    /// No socket (a try failed, or an open one ended), with a short reason for the screen. Reported once per outage.
    Down(String),
    Text(String),
    Binary(Vec<u8>),
}

#[derive(Debug, PartialEq, Eq)]
pub struct NotConnected;

/// A pipe the link thread polls beside its socket: whoever queues a message, stops the link or asks for a retry writes a byte.
struct Wake {
    r: i32,
    w: i32,
}

impl Wake {
    fn new() -> Option<Wake> {
        let mut fds = [0i32; 2];
        // SAFETY: `fds` is two ints, as pipe(2) wants.
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return None;
        }
        for fd in fds {
            // SAFETY: plain fcntl on descriptors we just made.
            unsafe {
                libc::fcntl(fd, libc::F_SETFL, libc::fcntl(fd, libc::F_GETFL) | libc::O_NONBLOCK);
                libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
            }
        }
        Some(Wake { r: fds[0], w: fds[1] })
    }

    fn ring(&self) {
        let b = 1u8;
        // SAFETY: one byte from a live local; a full pipe (EAGAIN) already means "woken".
        unsafe { libc::write(self.w, (&b as *const u8).cast(), 1) };
    }

    fn drain(&self) {
        let mut buf = [0u8; 64];
        // SAFETY: reads into a live local buffer; stops at EAGAIN.
        while unsafe { libc::read(self.r, buf.as_mut_ptr().cast(), buf.len()) } > 0 {}
    }
}

impl Drop for Wake {
    fn drop(&mut self) {
        // SAFETY: the two descriptors are ours and closed once.
        unsafe {
            libc::close(self.r);
            libc::close(self.w);
        }
    }
}

/// Waits until `fd` is readable (and writable too when `out`), `wake` rings, or `timeout` passes. Returns (socket readable or
/// closing, socket writable).
fn wait(fd: Option<i32>, out: bool, wake: &Wake, timeout: Duration) -> (bool, bool) {
    let mut fds = [
        libc::pollfd { fd: wake.r, events: libc::POLLIN, revents: 0 },
        libc::pollfd { fd: fd.unwrap_or(-1), events: libc::POLLIN | if out { libc::POLLOUT } else { 0 }, revents: 0 },
    ];
    let ms = timeout.as_millis().min(i32::MAX as u128) as i32;
    // SAFETY: two valid pollfd structs; a negative fd is ignored by poll.
    unsafe { libc::poll(fds.as_mut_ptr(), 2, ms) };
    if fds[0].revents != 0 {
        wake.drain();
    }
    let r = fds[1].revents;
    (r & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0, r & libc::POLLOUT != 0)
}

pub struct Link {
    tx: mpsc::Sender<Outgoing>,
    stop: Arc<AtomicBool>,
    kick: Arc<AtomicBool>,
    up: Arc<AtomicBool>,
    wake: Arc<Wake>,
    passes: Arc<AtomicU64>,
    handle: Option<JoinHandle<()>>,
}

impl Link {
    /// Starts the thread, which connects at once. `on_event` runs on that thread: it must not block.
    pub fn spawn(cfg: LinkConfig, on_event: Arc<dyn Fn(Incoming) + Send + Sync>) -> Link {
        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let kick = Arc::new(AtomicBool::new(false));
        let up = Arc::new(AtomicBool::new(false));
        let passes = Arc::new(AtomicU64::new(0));
        let wake = Arc::new(Wake::new().expect("pipe"));
        let t = Thread { cfg, rx, stop: stop.clone(), kick: kick.clone(), up: up.clone(), wake: wake.clone(), passes: passes.clone(), on_event };
        let handle = std::thread::Builder::new().name("voice-link".into()).spawn(move || t.run()).ok();
        Link { tx, stop, kick, up, wake, passes, handle }
    }

    /// How many times the open socket's loop has run (reads and sends); an idle link does not add to it (tests).
    pub fn passes(&self) -> u64 {
        self.passes.load(Ordering::Relaxed)
    }

    pub fn is_up(&self) -> bool {
        self.up.load(Ordering::Acquire)
    }

    /// Skips the rest of a backoff wait: try now.
    pub fn retry_now(&self) {
        self.kick.store(true, Ordering::Release);
        self.wake.ring();
    }

    /// Queues a message. Not while the socket is down: a frame from before a reconnect would be stale.
    pub fn send(&self, m: Outgoing) -> Result<(), NotConnected> {
        if !self.is_up() {
            return Err(NotConnected);
        }
        self.tx.send(m).map_err(|_| NotConnected)?;
        self.wake.ring();
        Ok(())
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.wake.ring();
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

enum Stream {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
}

impl Stream {
    fn tcp(&self) -> &TcpStream {
        match self {
            Stream::Plain(s) => s,
            Stream::Tls(s) => &s.sock,
        }
    }
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(s) => s.read(buf),
            Stream::Tls(s) => s.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(s) => s.write(buf),
            Stream::Tls(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Stream::Plain(s) => s.flush(),
            Stream::Tls(s) => s.flush(),
        }
    }
}

fn clock_is_set() -> bool {
    SystemTime::now().duration_since(UNIX_EPOCH).is_ok_and(|d| d.as_secs() >= CLOCK_FLOOR_SECS)
}

fn tls_config() -> Result<Arc<rustls::ClientConfig>, String> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(cfg))
}

struct Thread {
    cfg: LinkConfig,
    rx: mpsc::Receiver<Outgoing>,
    stop: Arc<AtomicBool>,
    kick: Arc<AtomicBool>,
    up: Arc<AtomicBool>,
    wake: Arc<Wake>,
    passes: Arc<AtomicU64>,
    on_event: Arc<dyn Fn(Incoming) + Send + Sync>,
}

impl Thread {
    fn run(self) {
        let tls = tls_config();
        let mut delay = Duration::from_secs(1);
        let mut announced_down = false;
        while !self.stop.load(Ordering::Acquire) {
            match self.connect(&tls) {
                Ok(mut ws) => {
                    delay = Duration::from_secs(1);
                    announced_down = false;
                    while self.rx.try_recv().is_ok() {}
                    self.up.store(true, Ordering::Release);
                    (self.on_event)(Incoming::Up);
                    let reason = self.serve(&mut ws);
                    self.up.store(false, Ordering::Release);
                    if !self.stop.load(Ordering::Acquire) {
                        (self.on_event)(Incoming::Down(reason));
                        announced_down = true;
                    }
                }
                Err(reason) => {
                    if !announced_down {
                        (self.on_event)(Incoming::Down(reason));
                        announced_down = true;
                    }
                }
            }
            // The backoff is one poll timeout; a stop or a retry-now rings the pipe.
            let until = Instant::now() + delay;
            while !self.stop.load(Ordering::Acquire) && !self.kick.swap(false, Ordering::AcqRel) {
                let left = until.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    break;
                }
                wait(None, false, &self.wake, left);
            }
            delay = (delay * 2).min(BACKOFF_MAX);
        }
    }

    fn connect(&self, tls: &Result<Arc<rustls::ClientConfig>, String>) -> Result<WebSocket<Stream>, String> {
        let mut request = self.cfg.url.as_str().into_client_request().map_err(|_| "bad address".to_owned())?;
        if let Some(t) = &self.cfg.bearer {
            let mut v = HeaderValue::from_str(&format!("Bearer {t}")).map_err(|_| "bad token".to_owned())?;
            v.set_sensitive(true);
            request.headers_mut().insert("Authorization", v);
        }
        let uri = request.uri().clone();
        let secure = match uri.scheme_str() {
            Some("wss") => true,
            Some("ws") => false,
            _ => return Err("bad address".into()),
        };
        let host = uri.host().ok_or("bad address")?.to_owned();
        let port = uri.port_u16().unwrap_or(if secure { 443 } else { 80 });
        let addrs = (host.as_str(), port).to_socket_addrs().map_err(|_| "no network".to_owned())?;
        let mut tcp = None;
        for a in addrs {
            if let Ok(s) = TcpStream::connect_timeout(&a, CONNECT_TIMEOUT) {
                tcp = Some(s);
                break;
            }
        }
        let tcp = tcp.ok_or("no network")?;
        let _ = tcp.set_nodelay(true);
        let _ = tcp.set_read_timeout(Some(HANDSHAKE_TIMEOUT));
        let _ = tcp.set_write_timeout(Some(HANDSHAKE_TIMEOUT));
        let stream = if secure {
            let cfg = tls.as_ref().map_err(|e| e.clone())?.clone();
            let name = rustls::pki_types::ServerName::try_from(host.clone()).map_err(|_| "bad address".to_owned())?;
            let conn = rustls::ClientConnection::new(cfg, name).map_err(|e| e.to_string())?;
            Stream::Tls(Box::new(rustls::StreamOwned::new(conn, tcp)))
        } else {
            Stream::Plain(tcp)
        };
        let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
        let mut attempt = tungstenite::client(request, stream);
        let ws = loop {
            match attempt {
                Ok((ws, _)) => break ws,
                Err(HandshakeError::Interrupted(mid)) => {
                    if Instant::now() > deadline {
                        return Err("no answer".into());
                    }
                    attempt = mid.handshake();
                }
                Err(HandshakeError::Failure(e)) => return Err(self.describe(e)),
            }
        };
        // From here the socket never blocks: `serve` sleeps in poll(2) and reads and writes what is ready.
        let _ = ws.get_ref().tcp().set_nonblocking(true);
        Ok(ws)
    }

    fn describe(&self, e: tungstenite::Error) -> String {
        use tungstenite::Error as E;
        match e {
            E::Http(resp) if resp.status().as_u16() == 401 => "signed out".into(),
            E::Http(resp) => format!("refused ({})", resp.status().as_u16()),
            E::Io(io) => {
                let text = io.to_string();
                if !clock_is_set() && (text.contains("certificate") || text.contains("Certificate")) {
                    "the clock is not set yet, so certificates cannot be checked".into()
                } else if text.contains("certificate") || text.contains("Certificate") {
                    "the server's certificate was refused".into()
                } else {
                    "no connection".into()
                }
            }
            E::Tls(_) => "no connection".into(),
            _ => "no connection".into(),
        }
    }

    /// Runs one open socket until it ends; returns why, for the screen. Event-driven: it sleeps in poll(2) on the socket and the
    /// wake pipe, with the dead-socket deadline as the only timeout, and reads until `WouldBlock` each time the socket is ready
    /// (which also takes what rustls already buffered from an earlier read).
    fn serve(&self, ws: &mut WebSocket<Stream>) -> String {
        use tungstenite::Error as E;
        let would_block = |e: &E| matches!(e, E::Io(io) if io.kind() == io::ErrorKind::WouldBlock);
        let fd = ws.get_ref().tcp().as_raw_fd();
        let mut heard = Instant::now();
        loop {
            self.passes.fetch_add(1, Ordering::Relaxed);
            if self.stop.load(Ordering::Acquire) {
                // What was queued before the stop (a cancel) still goes.
                while let Ok(m) = self.rx.try_recv() {
                    let _ = ws.send(into_message(m));
                }
                let _ = ws.close(None);
                // A last blocking-free flush; a full buffer at shutdown is not worth waiting for.
                let _ = ws.flush();
                return "closed".into();
            }
            while let Ok(m) = self.rx.try_recv() {
                if let Err(e) = ws.send(into_message(m)) {
                    // A full socket keeps the message queued inside tungstenite; the flush below finishes it.
                    if !would_block(&e) {
                        return "connection lost".into();
                    }
                }
            }
            loop {
                match ws.read() {
                    Ok(Message::Text(t)) => {
                        heard = Instant::now();
                        (self.on_event)(Incoming::Text(t.as_str().to_owned()))
                    }
                    Ok(Message::Binary(b)) => {
                        heard = Instant::now();
                        (self.on_event)(Incoming::Binary(b.to_vec()))
                    }
                    Ok(Message::Close(_)) => return "closed by the server".into(),
                    Ok(_) => heard = Instant::now(),
                    Err(ref e) if would_block(e) => break,
                    Err(E::Io(ref io)) if io.kind() == io::ErrorKind::TimedOut => break,
                    Err(E::ConnectionClosed | E::AlreadyClosed) => return "closed by the server".into(),
                    Err(_) => return "connection lost".into(),
                }
            }
            // The pong that answers a ping, and anything a full socket held back, goes now.
            let pending_out = match ws.flush() {
                Ok(()) => false,
                Err(ref e) if would_block(e) => true,
                Err(_) => return "connection lost".into(),
            };
            let left = (heard + DEAD_AFTER).saturating_duration_since(Instant::now());
            if left.is_zero() {
                return "no answer".into();
            }
            wait(Some(fd), pending_out, &self.wake, left);
        }
    }
}

fn into_message(m: Outgoing) -> Message {
    match m {
        Outgoing::Text(t) => Message::text(t),
        Outgoing::Binary(b) => Message::binary(b),
    }
}
