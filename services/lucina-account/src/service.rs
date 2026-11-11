//! `lucina-accountd`: the sign-in belongs to the machine. One service holds the device token, runs the one QR pairing and
//! answers every program over a Unix socket ([`lucina_account_link`] has the other side).
//!
//! The token is in this process's memory, with a copy in RAM under `/run` so that a restart of the service keeps it. On disk
//! only when [`Config::keep_on_disk`] says so: the owner decides that, and /data is not encrypted. With it off, a token left on
//! /data by anything earlier is removed.
//!
//! A pairing runs while at least one program shows the sign-in screen, so a machine nobody looks at does not use up the
//! server's sign-in budget. A code already on a screen is still polled until it expires, because an approval comes only once
//! and would otherwise leave a device on the server that nobody holds.
//!
//! Never logs the token or the poll token.
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use lucina_account_link::credential::{self, Token};
use lucina_account_link::{readable, Pairing, Reply, Status};
use serde::Deserialize;

use crate::client::{ApiError, Client};
use crate::signin::{Phase, SignIn, Step, DEVICE_NAME};

/// How long a kept token waits between tries to ask the server about it, while the server cannot be reached.
const CONFIRM_RETRY: Duration = Duration::from_secs(15);
/// How long programs wait for /data before they may pair anyway. A token found later still counts.
const DATA_WAIT: Duration = Duration::from_secs(20);
/// The least time between two questions to the server about the same token, however many programs report it.
const CHECK_EVERY: Duration = Duration::from_secs(5);
const CONNECTION_STACK: usize = 128 * 1024;

#[derive(Clone, Debug)]
pub struct Config {
    pub socket: PathBuf,
    pub ram: PathBuf,
    pub disk: PathBuf,
    /// `data-ready`, `data-failed` and `data-expected` are here (`/run/lucina`).
    pub run: PathBuf,
    pub keep_on_disk: bool,
    /// Instead of [`crate::client::BASE_ENV`]; the tests' fake server.
    pub base: Option<String>,
    /// [`CONFIRM_RETRY`]; shorter in tests.
    pub confirm_retry: Duration,
}

impl Config {
    /// The paths under `root`, which is `/` on a real machine.
    pub fn under(root: &Path, keep_on_disk: bool) -> Config {
        let at = |p: &str| root.join(p.trim_start_matches('/'));
        Config {
            socket: at(lucina_account_link::SOCKET),
            ram: at(credential::RAM_PATH),
            disk: at(credential::DATA_PATH),
            run: at("/run/lucina"),
            keep_on_disk,
            base: None,
            confirm_retry: CONFIRM_RETRY,
        }
    }

    fn client(&self, token: Option<Token>) -> Option<Client> {
        match &self.base {
            Some(b) => Client::new(b, token),
            None => Client::from_env(token),
        }
    }
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// When a kept token was written: the pairing, since the service never rewrites a file that already holds its token.
fn written_at(path: &Path) -> Option<u64> {
    let t = std::fs::metadata(path).and_then(|m| m.modified()).ok()?;
    t.duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs())
}

#[derive(Default)]
struct Shared {
    starting: bool,
    checking: bool,
    token: Option<Token>,
    device_name: Option<String>,
    device_id: Option<String>,
    on_disk: bool,
    paired_at: Option<u64>,
    confirmed_at: Option<u64>,
    pairing: Option<Pairing>,
    /// Open connections that show the sign-in screen.
    wanted: usize,
    new_code: bool,
    last_check: Option<Instant>,
    /// Moves at every change watchers should hear of.
    generation: u64,
}

impl Shared {
    fn status(&self) -> Status {
        if self.starting {
            Status::Starting
        } else if self.checking {
            Status::Checking
        } else if self.token.is_some() {
            Status::SignedIn {
                device_name: self.device_name.clone(),
                on_disk: self.on_disk,
                device_id: self.device_id.clone(),
                paired_at: self.paired_at,
                confirmed_at: self.confirmed_at,
            }
        } else if let Some(pairing) = &self.pairing {
            Status::SigningIn { pairing: pairing.clone() }
        } else {
            Status::SignedOut
        }
    }
}

struct Service {
    cfg: Config,
    shared: Mutex<Shared>,
    changed: Condvar,
}

/// Runs in the threads it starts; the handle says where to connect. Ends with the process.
pub struct Running(Arc<Service>);

impl Running {
    pub fn socket(&self) -> &Path {
        &self.0.cfg.socket
    }
}

/// Binds the socket and starts. Fails when another service is already answering on it.
pub fn start(cfg: Config) -> io::Result<Running> {
    if let Some(dir) = cfg.socket.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if UnixStream::connect(&cfg.socket).is_ok() {
        return Err(io::Error::new(io::ErrorKind::AddrInUse, "another lucina-accountd is running"));
    }
    let _ = std::fs::remove_file(&cfg.socket);
    // SAFETY: umask only sets this process's mask. The socket is the only thing that goes through it before the chmod below.
    let old = unsafe { libc::umask(0o077) };
    let listener = UnixListener::bind(&cfg.socket);
    // SAFETY: as above.
    unsafe { libc::umask(old) };
    let listener = listener?;
    std::fs::set_permissions(&cfg.socket, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;

    let service = Arc::new(Service {
        cfg,
        shared: Mutex::new(Shared { starting: true, ..Shared::default() }),
        changed: Condvar::new(),
    });
    let s = service.clone();
    std::thread::spawn(move || s.storage());
    let s = service.clone();
    std::thread::spawn(move || s.pairing_loop());
    let s = service.clone();
    std::thread::spawn(move || {
        for conn in listener.incoming().flatten() {
            let s = s.clone();
            let _ = std::thread::Builder::new().stack_size(CONNECTION_STACK).spawn(move || {
                let _ = s.serve(conn);
            });
        }
    });
    Ok(Running(service))
}

/// A kept token, or nothing. A file that holds no token (cut short, overwritten, empty) is removed: that is "signed out",
/// and the next approval writes a whole one.
fn load_or_drop(path: &Path, place: &str) -> Option<Token> {
    match credential::load(path) {
        Ok(found) => found,
        Err(e) => {
            eprintln!("lucina-accountd: the token {place} is damaged ({}); signed out", e.kind());
            if let Err(e) = credential::remove(path) {
                eprintln!("lucina-accountd: the damaged token {place} could not be removed: {e}");
            }
            None
        }
    }
}

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
enum Data {
    /// No /data on this boot.
    None,
    Waiting,
    Ready,
}

/// The contract of `lucina-data` (docs/research/juno-disk-root.md).
fn data_state(run: &Path) -> Data {
    if run.join("data-failed").exists() || !run.join("data-expected").exists() {
        Data::None
    } else if run.join("data-ready").exists() {
        Data::Ready
    } else {
        Data::Waiting
    }
}

#[derive(Deserialize)]
struct Request {
    op: String,
    #[serde(default)]
    sign_in: bool,
}

impl Service {
    fn lock(&self) -> MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn touch(&self, g: &mut Shared) {
        g.generation += 1;
        self.changed.notify_all();
    }

    /// What a boot finds: RAM first (the service restarted), then /data once it is up.
    fn storage(self: Arc<Self>) {
        let cfg = &self.cfg;
        let found = load_or_drop(&cfg.ram, "in RAM");
        let from_ram = found.is_some();
        {
            let mut g = self.lock();
            g.token = found;
            if from_ram {
                g.paired_at = written_at(&cfg.ram);
                g.starting = false;
                self.touch(&mut g);
            }
        }
        if from_ram {
            self.clone().confirm();
        }
        let waited_from = Instant::now();
        loop {
            match data_state(&cfg.run) {
                Data::None => break,
                Data::Ready => {
                    self.clone().data_is_up();
                    break;
                }
                Data::Waiting if waited_from.elapsed() >= DATA_WAIT => {
                    let mut g = self.lock();
                    if g.starting {
                        eprintln!("lucina-accountd: /data is taking long; programs may sign in meanwhile");
                        g.starting = false;
                        self.touch(&mut g);
                    }
                }
                Data::Waiting => {}
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let mut g = self.lock();
        g.starting = false;
        self.touch(&mut g);
    }

    fn data_is_up(self: Arc<Self>) {
        let cfg = &self.cfg;
        if !cfg.keep_on_disk {
            if cfg.disk.exists() {
                match credential::remove(&cfg.disk) {
                    Ok(()) => eprintln!("lucina-accountd: removed a token from /data: it is kept in RAM on this machine"),
                    Err(e) => eprintln!("lucina-accountd: a token on /data could not be removed: {e}"),
                }
            }
            return;
        }
        let held = self.lock().token.clone();
        match held {
            // Signed in while /data was still coming up, or a restart of the service.
            Some(token) => {
                self.keep_on_disk(&token);
                // The file on /data is older than the copy in RAM when the service restarted.
                let mut g = self.lock();
                if g.on_disk {
                    if let Some(t) = written_at(&cfg.disk) {
                        g.paired_at = Some(g.paired_at.map_or(t, |p| p.min(t)));
                        self.touch(&mut g);
                    }
                }
            }
            None => match load_or_drop(&cfg.disk, "on /data") {
                Some(token) => {
                    if let Err(e) = credential::save(&cfg.ram, &token) {
                        eprintln!("lucina-accountd: no copy in RAM: {e}");
                    }
                    let mut g = self.lock();
                    g.token = Some(token);
                    g.on_disk = true;
                    g.paired_at = written_at(&cfg.disk);
                    self.touch(&mut g);
                    drop(g);
                    eprintln!("lucina-accountd: signed in with the token kept on /data");
                    self.clone().confirm();
                }
                None => {}
            },
        }
    }

    fn keep_on_disk(&self, token: &Token) {
        // Already there: leave it, so its time stays the pairing's.
        let saved = if matches!(credential::load(&self.cfg.disk), Ok(Some(ref t)) if t == token) {
            Ok(())
        } else {
            credential::save(&self.cfg.disk, token)
        };
        match saved {
            Ok(()) => {
                let mut g = self.lock();
                g.on_disk = true;
                self.touch(&mut g);
            }
            Err(e) => eprintln!("lucina-accountd: the token could not be kept on /data, so it stays in RAM: {e}"),
        }
    }

    /// A token kept from before this boot: the server may have revoked it meanwhile. As the iPhone does at launch, ask
    /// `GET /v1/session`; "not authenticated" signs the machine out. No answer is not a no: the network often comes up after
    /// the service, so it asks again until the server answers, and programs use the token meanwhile.
    fn confirm(self: Arc<Self>) {
        std::thread::spawn(move || loop {
            let Some(token) = self.lock().token.clone() else { return };
            let Some(client) = self.cfg.client(Some(token.clone())) else { return };
            let answer = client.session();
            let mut g = self.lock();
            // Signed out, or signed in again, while it asked: that answer is about another token.
            if g.token.as_ref() != Some(&token) {
                return;
            }
            match answer {
                Ok(s) if s.authenticated => {
                    g.device_name = s.device_name;
                    g.device_id = s.device_id;
                    g.confirmed_at = Some(now_secs());
                    self.touch(&mut g);
                    return;
                }
                Ok(_) | Err(ApiError::Unauthorized(_)) => {
                    eprintln!("lucina-accountd: the server no longer takes the kept token (revoked?); signed out");
                    self.forget(&mut g);
                    return;
                }
                Err(e) => {
                    drop(g);
                    eprintln!("lucina-accountd: could not ask the server about the kept token yet: {e}");
                    std::thread::sleep(self.cfg.confirm_retry);
                }
            }
        });
    }

    /// The name on the server, for Settings. Nothing depends on it.
    fn look_up_device(&self) {
        let Some(token) = self.lock().token.clone() else { return };
        let Some(client) = self.cfg.client(Some(token)) else { return };
        if let Ok(session) = client.session() {
            let mut g = self.lock();
            if session.authenticated {
                g.device_name = session.device_name;
                g.device_id = session.device_id;
                g.confirmed_at = Some(now_secs());
                self.touch(&mut g);
            }
        }
    }

    fn adopt(&self, token: Token) {
        // Even without a file the token stays in this process: it comes only once.
        if let Err(e) = credential::save(&self.cfg.ram, &token) {
            eprintln!("lucina-accountd: no copy in RAM: {e}");
        }
        {
            let mut g = self.lock();
            g.token = Some(token.clone());
            g.pairing = None;
            // The server just handed it over: that is its word that the token is good.
            let now = now_secs();
            g.paired_at = Some(now);
            g.confirmed_at = Some(now);
            g.device_id = None;
            g.device_name = None;
            self.touch(&mut g);
        }
        eprintln!("lucina-accountd: signed in");
        if self.cfg.keep_on_disk && data_state(&self.cfg.run) == Data::Ready {
            self.keep_on_disk(&token);
        }
        self.look_up_device();
    }

    /// Everything of this account on this machine: the token wherever it is, and what apps cached beside it.
    fn forget(&self, g: &mut Shared) {
        g.token = None;
        g.device_name = None;
        g.device_id = None;
        g.on_disk = false;
        g.paired_at = None;
        g.confirmed_at = None;
        if let Err(e) = credential::remove(&self.cfg.disk) {
            eprintln!("lucina-accountd: the token on /data could not be removed: {e}");
        }
        if let Some(dir) = self.cfg.ram.parent() {
            if let Ok(files) = std::fs::read_dir(dir) {
                for f in files.flatten() {
                    let _ = std::fs::remove_file(f.path());
                }
            }
        }
        self.touch(g);
    }

    /// A program was told 401. The token may be older than the one held here, or the answer odd: the server decides.
    fn check(self: Arc<Self>, last: Option<Instant>) {
        std::thread::spawn(move || {
            if let Some(last) = last {
                std::thread::sleep(CHECK_EVERY.saturating_sub(last.elapsed()));
            }
            let token = self.lock().token.clone();
            let answer = token.and_then(|t| self.cfg.client(Some(t))).map(|c| c.session());
            let mut g = self.lock();
            g.checking = false;
            match answer {
                Some(Ok(s)) if s.authenticated => {
                    eprintln!("lucina-accountd: the server still takes the token");
                    g.confirmed_at = Some(now_secs());
                }
                Some(Ok(_)) | Some(Err(ApiError::Unauthorized(_))) => {
                    eprintln!("lucina-accountd: the server no longer takes the token; signed out");
                    self.forget(&mut g);
                }
                // No answer tells nothing. The program asks again when it next gets a 401.
                Some(Err(e)) => eprintln!("lucina-accountd: could not ask about the token: {e}"),
                None => {}
            }
            self.touch(&mut g);
        });
    }

    fn pairing_loop(&self) {
        let Some(client) = self.cfg.client(None) else {
            eprintln!("lucina-accountd: {} must be https, or http to this machine", crate::client::BASE_ENV);
            return;
        };
        let mut signin: Option<SignIn> = None;
        let mut shown: Option<Pairing> = None;
        loop {
            let (wanted, new_code) = {
                let mut g = self.lock();
                loop {
                    let idle = g.starting || g.token.is_some() || (g.wanted == 0 && signin.is_none());
                    if !idle {
                        break;
                    }
                    if g.token.is_some() || g.starting {
                        signin = None;
                        shown = None;
                    }
                    g = self.changed.wait(g).unwrap_or_else(|p| p.into_inner());
                }
                (g.wanted, std::mem::take(&mut g.new_code))
            };
            let now = Instant::now();
            // Nobody is looking, and no approval can still arrive.
            if wanted == 0 && signin.as_ref().is_none_or(|s| s.live_until().is_none_or(|t| now >= t)) {
                signin = None;
                shown = None;
                let mut g = self.lock();
                g.pairing = None;
                self.touch(&mut g);
                continue;
            }
            let s = signin.get_or_insert_with(|| SignIn::new(DEVICE_NAME));
            if new_code {
                s.again(now);
            }
            if let Step::Token(token) = s.tick(&client, now) {
                signin = None;
                shown = None;
                self.adopt(token);
                continue;
            }
            let pairing = wire(s, now);
            if shown.as_ref().map(without_clock) != Some(without_clock(&pairing)) {
                shown = Some(pairing.clone());
                let mut g = self.lock();
                g.pairing = Some(pairing);
                self.touch(&mut g);
            } else {
                // Late joiners get the right countdown; nobody is woken for it.
                self.lock().pairing = Some(pairing);
            }
            let g = self.lock();
            let generation = g.generation;
            let _ = self.changed.wait_timeout_while(g, Duration::from_millis(200), |g| g.generation == generation);
        }
    }

    fn serve(self: Arc<Self>, conn: UnixStream) -> io::Result<()> {
        let mut reader = BufReader::new(conn.try_clone()?);
        let mut writer = conn;
        let mut line = String::new();
        while reader.read_line(&mut line)? > 0 {
            let request: Option<Request> = serde_json::from_str(&line).ok();
            line.clear();
            let Some(request) = request else { continue };
            match request.op.as_str() {
                "watch" => return self.watch(reader.get_ref(), &mut writer, request.sign_in),
                "token" => {
                    let g = self.lock();
                    let usable = !g.starting && !g.checking;
                    let reply = Reply { status: g.status(), token: g.token.as_ref().filter(|_| usable).map(|t| t.expose().to_owned()) };
                    drop(g);
                    send(&mut writer, &reply)?;
                }
                "status" => self.answer(&mut writer)?,
                "rejected" => {
                    let mut g = self.lock();
                    if g.token.is_some() && !g.checking {
                        g.checking = true;
                        self.touch(&mut g);
                        let last = g.last_check.replace(Instant::now());
                        drop(g);
                        self.clone().check(last);
                    } else {
                        drop(g);
                    }
                    self.answer(&mut writer)?;
                }
                "sign_out" => {
                    // The server has no way for a device to end itself: it stays in the list until it is revoked there.
                    let mut g = self.lock();
                    g.pairing = None;
                    self.forget(&mut g);
                    drop(g);
                    eprintln!("lucina-accountd: signed out here; the device stays on the server until it is revoked there");
                    self.answer(&mut writer)?;
                }
                "new_code" => {
                    let mut g = self.lock();
                    g.new_code = true;
                    self.touch(&mut g);
                    drop(g);
                    self.answer(&mut writer)?;
                }
                _ => self.answer(&mut writer)?,
            }
        }
        Ok(())
    }

    fn answer(&self, writer: &mut UnixStream) -> io::Result<()> {
        let status = self.lock().status();
        send(writer, &Reply { status, token: None })
    }

    fn watch(&self, peer: &UnixStream, writer: &mut UnixStream, sign_in: bool) -> io::Result<()> {
        struct Wanted<'a>(&'a Service, bool);
        impl Drop for Wanted<'_> {
            fn drop(&mut self) {
                if self.1 {
                    let mut g = self.0.lock();
                    g.wanted -= 1;
                    self.0.touch(&mut g);
                }
            }
        }
        if sign_in {
            let mut g = self.lock();
            g.wanted += 1;
            self.touch(&mut g);
        }
        let _wanted = Wanted(self, sign_in);
        let mut said: Option<Status> = None;
        let mut seen = u64::MAX;
        loop {
            let status = {
                let mut g = self.lock();
                if g.generation == seen {
                    g = self.changed.wait_timeout(g, Duration::from_millis(500)).unwrap_or_else(|p| p.into_inner()).0;
                }
                seen = g.generation;
                g.status()
            };
            if said.as_ref().map(status_without_clock) != Some(status_without_clock(&status)) {
                send(writer, &Reply { status: status.clone(), token: None })?;
                said = Some(status);
            }
            // A watcher only listens: anything readable here is the other end going away.
            if readable(peer, Duration::ZERO)? {
                return Ok(());
            }
        }
    }
}

fn send(writer: &mut UnixStream, reply: &Reply) -> io::Result<()> {
    let mut line = serde_json::to_vec(reply).map_err(io::Error::other)?;
    line.push(b'\n');
    writer.write_all(&line)?;
    use zeroize::Zeroize;
    line.zeroize();
    Ok(())
}

fn wire(s: &SignIn, now: Instant) -> Pairing {
    match s.phase(now) {
        Phase::Ready { short_code, seconds_left, .. } => {
            Pairing::Ready { approve_url: s.approve_url().unwrap_or_default().to_owned(), short_code, seconds_left }
        }
        Phase::Denied => Pairing::Denied,
        Phase::Paused => Pairing::Paused,
        Phase::Unavailable { message, rate_limited } => Pairing::Unavailable { message, rate_limited },
        Phase::Loading | Phase::SignedIn => Pairing::Loading,
    }
}

fn without_clock(p: &Pairing) -> Pairing {
    match p {
        Pairing::Ready { approve_url, short_code, .. } => {
            Pairing::Ready { approve_url: approve_url.clone(), short_code: short_code.clone(), seconds_left: 0 }
        }
        other => other.clone(),
    }
}

/// Watchers count the seconds themselves; only a new code is news.
fn status_without_clock(s: &Status) -> Status {
    match s {
        Status::SigningIn { pairing } => Status::SigningIn { pairing: without_clock(pairing) },
        other => other.clone(),
    }
}
