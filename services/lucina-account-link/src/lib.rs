//! How Juno's programs reach `lucina-accountd`, the system service that holds the sign-in to the owner's Secretary account.
//!
//! The account belongs to the machine, not to an app: one QR sign-in, one device on the server, every program signed in by
//! it. Programs ask the service over a Unix socket (one JSON line each way, mode 0600) for the token when they need one, and
//! tell it when the server refused that token. They never keep it in a file. The service asks the server before it believes
//! them, so an app holding an old token cannot sign the machine out.
//!
//! ```no_run
//! # use lucina_account_link as link;
//! match link::token() {
//!     Some(_token) => { /* signed in */ }
//!     None => { /* show ui::SignInView, which watches the service and says when the machine is signed in */ }
//! }
//! ```
//! `lucina_account::system::client()` gives a ready `Client`. This crate has no TLS in it, so the shell can use it.
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use qrcode::{Color, EcLevel, QrCode};
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

pub mod credential;
#[cfg(feature = "ui")]
pub mod ui;

pub use credential::Token;

/// The service's socket.
pub const SOCKET: &str = "/run/lucina/account.sock";
/// Set by demos that run the service under another root.
pub const SOCKET_ENV: &str = "LUCINA_ACCOUNTD_SOCKET";
/// What a question waits for at most: the service answers from memory, and only a hung one takes this long.
const ANSWER: Duration = Duration::from_secs(2);
/// How long [`token`] waits for a service that is still finding out, at boot or while it asks the server about a token.
const SETTLE: Duration = Duration::from_secs(1);

/// The sign-in's state, the same for every program.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Status {
    /// Waiting for /data at boot, to see whether a token is kept there.
    Starting,
    /// A program's request was refused, and the service is asking the server about this token.
    Checking,
    /// Nobody is signed in, and no program is showing the sign-in screen.
    SignedOut,
    SigningIn { pairing: Pairing },
    SignedIn {
        device_name: Option<String>,
        /// The token survives a restart. False means RAM only: the phone has to approve again after one.
        on_disk: bool,
        /// Which device the server knows this machine as (`dev_…`). The server has one account, the owner's, and its
        /// `GET /v1/session` names only the device: this is the "who" there is.
        #[serde(default)]
        device_id: Option<String>,
        /// Unix seconds the phone approved this sign-in. For a token kept from an older service, the file's time.
        #[serde(default)]
        paired_at: Option<u64>,
        /// Unix seconds the server last said the token is good: the pairing, the boot check, a check after a refusal.
        #[serde(default)]
        confirmed_at: Option<u64>,
    },
}

/// The one pairing in progress, whichever programs are showing it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum Pairing {
    Loading,
    Ready { approve_url: String, short_code: Option<String>, seconds_left: u64 },
    Denied,
    Paused,
    Unavailable { message: String, rate_limited: bool },
}

#[derive(Serialize, Deserialize)]
pub struct Reply {
    pub status: Status,
    /// Only in the answer to `token`, only when signed in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

/// A square QR code without the quiet zone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Qr {
    pub width: usize,
    dark: Vec<bool>,
}

/// Modules of quiet zone to leave around the code.
pub const QUIET: usize = 4;

impl Qr {
    pub fn encode(text: &str) -> Option<Qr> {
        let code = QrCode::with_error_correction_level(text.as_bytes(), EcLevel::M).ok()?;
        let width = code.width();
        let dark = code.to_colors().into_iter().map(|c| c == Color::Dark).collect();
        Some(Qr { width, dark })
    }

    pub fn is_dark(&self, x: usize, y: usize) -> bool {
        x < self.width && y < self.width && self.dark[y * self.width + x]
    }
}

/// What the sign-in screen shows.
#[derive(Debug, PartialEq, Eq)]
pub enum Phase {
    Loading,
    Ready { qr: Box<Qr>, short_code: Option<String>, seconds_left: u64 },
    /// Declined on the phone. A button asks for a new code.
    Denied,
    /// Too many codes went unanswered. A button asks for a new code.
    Paused,
    /// The server said no or cannot be reached; the machine tries again by itself.
    Unavailable { message: String, rate_limited: bool },
    SignedIn,
}

/// A connection to one service: [`Link::system`], or a test's own.
#[derive(Clone, Debug)]
pub struct Link {
    socket: PathBuf,
}

impl Link {
    /// This machine's service: [`SOCKET_ENV`] or [`SOCKET`].
    pub fn system() -> Link {
        Link::at(std::env::var_os(SOCKET_ENV).map_or_else(|| PathBuf::from(SOCKET), PathBuf::from))
    }

    pub fn at(socket: impl AsRef<Path>) -> Link {
        Link { socket: socket.as_ref().to_owned() }
    }

    fn ask(&self, op: &str) -> io::Result<Reply> {
        let mut w = Lines::connect(&self.socket)?;
        w.send(op, false)?;
        let mut line = w.next_line(ANSWER)?.ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "no answer"))?;
        let reply = serde_json::from_slice(&line).map_err(io::Error::other);
        // The answer to `token` holds it.
        line.zeroize();
        reply
    }

    pub fn status(&self) -> io::Result<Status> {
        Ok(self.ask("status")?.status)
    }

    /// The token, when the machine is signed in. `None` when it is not, or when the service is not there. Waits at most
    /// [`SETTLE`] while the service is finding out, so call it on a thread if a second matters.
    pub fn token(&self) -> Option<Token> {
        let until = Instant::now() + SETTLE;
        loop {
            let mut reply = self.ask("token").ok()?;
            if let Some(t) = &mut reply.token {
                let token = Token::parse(t);
                t.zeroize();
                return token;
            }
            if !matches!(reply.status, Status::Starting | Status::Checking) || Instant::now() >= until {
                return None;
            }
            std::thread::sleep(Duration::from_millis(30));
        }
    }

    /// The server answered 401 to the token this program holds. The service checks with the server, and either signs the
    /// machine out or keeps the token. Returns at once: then ask [`Link::token`] again, or show the sign-in screen, which
    /// says "signed in" right away if the token was good.
    pub fn report_rejected(&self) -> io::Result<()> {
        self.ask("rejected").map(|_| ())
    }

    /// Signs the machine out for every program: forgets the token here and every app's cached account data. The device stays
    /// in the server's list until it is revoked there; the server has no way for a device to end itself.
    pub fn sign_out(&self) -> io::Result<()> {
        self.ask("sign_out").map(|_| ())
    }

    /// The sign-in screen's button: a new round of codes.
    pub fn new_code(&self) -> io::Result<()> {
        self.ask("new_code").map(|_| ())
    }

    /// Follow the state. With `sign_in`, this connection also asks for a pairing to run, for as long as it stays open.
    pub fn watch(&self, sign_in: bool) -> io::Result<Watch> {
        let mut lines = Lines::connect(&self.socket)?;
        lines.send("watch", sign_in)?;
        Ok(Watch { lines })
    }

    /// `changed` on a thread of its own at every change, and once at the start; reconnects when the service restarts. `None`
    /// while the service is not there. Ends when the handle is dropped.
    pub fn follow(&self, changed: impl Fn(Option<Status>) + Send + 'static) -> Following {
        let stop = Arc::new(AtomicBool::new(false));
        let (link, flag) = (self.clone(), stop.clone());
        std::thread::spawn(move || {
            let mut known = false;
            while !flag.load(Ordering::Relaxed) {
                if let Ok(mut w) = link.watch(false) {
                    known = true;
                    while !flag.load(Ordering::Relaxed) {
                        match w.next(Duration::from_millis(500)) {
                            Ok(Some(s)) => changed(Some(s)),
                            Ok(None) => {}
                            Err(_) => break,
                        }
                    }
                }
                if known {
                    known = false;
                    changed(None);
                }
                std::thread::sleep(Duration::from_secs(1));
            }
        });
        Following { stop }
    }
}

/// Ends [`Link::follow`].
pub struct Following {
    stop: Arc<AtomicBool>,
}

impl Drop for Following {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// The state as it changes.
pub struct Watch {
    lines: Lines,
}

impl Watch {
    /// `Ok(None)` when nothing changed for `wait`. An error means the service went away.
    pub fn next(&mut self, wait: Duration) -> io::Result<Option<Status>> {
        match self.lines.next_line(wait)? {
            None => Ok(None),
            Some(line) => Ok(Some(serde_json::from_slice::<Reply>(&line).map_err(io::Error::other)?.status)),
        }
    }
}

/// One JSON line at a time. `poll` rather than a socket timeout, so a line's arrival and a closed peer look the same on every kernel.
struct Lines {
    stream: UnixStream,
    buf: Vec<u8>,
}

impl Lines {
    fn connect(path: &Path) -> io::Result<Lines> {
        Ok(Lines { stream: UnixStream::connect(path)?, buf: Vec::new() })
    }

    fn send(&mut self, op: &str, sign_in: bool) -> io::Result<()> {
        let line = serde_json::json!({"op": op, "sign_in": sign_in}).to_string() + "\n";
        self.stream.write_all(line.as_bytes())
    }

    fn next_line(&mut self, wait: Duration) -> io::Result<Option<Vec<u8>>> {
        let until = Instant::now() + wait;
        loop {
            if let Some(i) = self.buf.iter().position(|b| *b == b'\n') {
                let rest = self.buf.split_off(i + 1);
                let mut line = std::mem::replace(&mut self.buf, rest);
                line.pop();
                return Ok(Some(line));
            }
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() || !readable(&self.stream, left)? {
                return Ok(None);
            }
            let mut chunk = [0u8; 4096];
            let n = self.stream.read(&mut chunk)?;
            if n == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            self.buf.extend_from_slice(&chunk[..n]);
            chunk.zeroize();
        }
    }
}

/// Has the stream something to read, a closed peer included? `pub` for the service, which watches for that on its side.
pub fn readable(stream: &UnixStream, wait: Duration) -> io::Result<bool> {
    let mut fd = libc::pollfd { fd: stream.as_raw_fd(), events: libc::POLLIN, revents: 0 };
    // SAFETY: one valid pollfd, for the length of the call.
    let n = unsafe { libc::poll(&mut fd, 1, wait.as_millis().min(i32::MAX as u128) as libc::c_int) };
    match n {
        n if n > 0 => Ok(true),
        0 => Ok(false),
        _ => {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted { Ok(false) } else { Err(e) }
        }
    }
}

/// The token for this program. With [`credential::PATH_ENV`] set (tests and demos without the service), that file.
pub fn token() -> Option<Token> {
    match std::env::var_os(credential::PATH_ENV) {
        Some(p) => credential::load(Path::new(&p)).ok().flatten(),
        None => Link::system().token(),
    }
}

/// The server refused this program's token; see [`Link::report_rejected`].
pub fn report_rejected() {
    match std::env::var_os(credential::PATH_ENV) {
        Some(p) => {
            let _ = credential::remove(Path::new(&p));
        }
        None => {
            let _ = Link::system().report_rejected();
        }
    }
}
