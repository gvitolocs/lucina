//! Juno's screen in a browser. voxl (github.com/tembo/voxl, MIT, pinned at 47bb94ec4d25) streams a desktop as H.264 stripes
//! over one WebSocket to a page that decodes them with WebCodecs. This crate serves voxl's browser client unchanged (`web/`,
//! see its README) and implements voxl's server protocol on plain std threads, for the static programs Juno runs: the hello,
//! pings, the 16-byte video header, acks and flow control, paint-over, input, `/screenshot`, `/wait` and `/stats`.
//! voxl is the source of truth for behaviour; each module says which of its files it follows.
//!
//! Not here (voxl has them): audio, microphone, clipboard, file transfer, recordings, resizing, cursor images.

pub mod capture;
mod hub;
pub mod input;
pub mod keysym;
pub mod login;
pub mod net;
mod server;
pub mod video;
pub mod wait;
pub mod ws;

use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub use net::Lan;

/// The screen: XRGB8888 rows. Behind a trait so that the framebuffer reader can be swapped, and for tests.
pub trait Screen: Send {
    fn size(&self) -> (usize, usize);
    /// Row `y`; `out` is the row's width.
    fn read_row(&mut self, y: usize, out: &mut [u32]);
}

/// Where the viewer's input goes. Codes are evdev's (`linux/input-event-codes.h`).
pub trait Input: Send {
    /// The pointer to this position on the screen.
    fn move_to(&mut self, x: i32, y: i32);
    /// `BTN_LEFT` and friends.
    fn button(&mut self, code: u16, down: bool);
    /// Wheel notches: positive `dy` scrolls down, positive `dx` right (the browser's and voxl's sense, not evdev's).
    fn wheel(&mut self, dx: i32, dy: i32);
    fn key(&mut self, code: u16, down: bool);
}

/// The port viewers dial by default.
pub const PORT: u16 = 6901;
/// Connections served at once; one more is answered 503.
pub const MAX_CONNECTIONS: usize = 4;
/// A connection over the limit waits this long for a slot before its 503: a page loads its scripts, icon and font in a burst.
const SLOT_WAIT: Duration = Duration::from_secs(1);

/// One address to listen on, and the network whose machines it answers.
#[derive(Clone, Copy, Debug)]
pub struct Listen {
    pub addr: SocketAddr,
    pub lan: Lan,
}

pub struct Config {
    pub listen: Vec<Listen>,
    /// Only this machine, instead of the whole network.
    pub only: Option<IpAddr>,
    pub read_only: bool,
    /// The page's title.
    pub title: String,
    /// What a viewer must present: `?token=`, Bearer or Basic. Never logged here.
    pub password: String,
}

/// A running host. Dropping it does not stop it; `stop` does.
pub struct Server {
    hub: Arc<hub::Hub>,
    addrs: Vec<SocketAddr>,
    threads: Vec<JoinHandle<()>>,
}

struct Slot(Arc<AtomicUsize>);

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

fn take_slot(active: &Arc<AtomicUsize>) -> Option<Slot> {
    let start = Instant::now();
    loop {
        let n = active.load(Ordering::SeqCst);
        if n < MAX_CONNECTIONS {
            if active.compare_exchange(n, n + 1, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
                return Some(Slot(active.clone()));
            }
            continue;
        }
        if start.elapsed() >= SLOT_WAIT {
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn refuse_busy(mut s: TcpStream) {
    use std::io::{Read, Write};
    // Reads what the client sent first: closing a socket with unread data resets it, and the 503 would be lost.
    let _ = s.set_read_timeout(Some(Duration::from_millis(200)));
    let _ = s.read(&mut [0u8; 4096]);
    let _ = s.set_write_timeout(Some(Duration::from_secs(2)));
    let body = "busy: too many connections";
    let _ = write!(
        s,
        "HTTP/1.1 503 Service Unavailable\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nRetry-After: 1\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = s.shutdown(std::net::Shutdown::Write);
    let _ = s.read(&mut [0u8; 4096]);
}

impl Server {
    /// Binds every address, then starts capturing and serving.
    pub fn start(cfg: Config, screen: Box<dyn Screen>, input: Box<dyn Input>) -> Result<Server, String> {
        let (w, h) = screen.size();
        if w == 0 || h == 0 || w > 4096 || h > 4096 {
            return Err(format!("a {w}x{h} screen cannot be streamed"));
        }
        let mut listeners = Vec::new();
        for l in &cfg.listen {
            let listener = TcpListener::bind(l.addr).map_err(|e| format!("{}: {e}", l.addr))?;
            let addr = listener.local_addr().map_err(|e| e.to_string())?;
            listeners.push((listener, addr, l.lan));
        }
        let hub = hub::Hub::new(w as u32, h as u32, video::stripe_layout(h as u32, video::STRIPES), cfg.read_only, input::Keys::new(input));
        let login = Arc::new(Mutex::new(login::Login::new(cfg.password)));
        let assets = Arc::new(server::Assets { index: server::index_html(&cfg.title) });
        let active = Arc::new(AtomicUsize::new(0));
        let mut threads = Vec::new();
        let h2 = hub.clone();
        threads.push(spawn("desk-web-video", move || video::run(h2, screen))?);
        let mut addrs = Vec::new();
        for (listener, addr, lan) in listeners {
            addrs.push(addr);
            let (hub, login, assets, active, only) = (hub.clone(), login.clone(), assets.clone(), active.clone(), cfg.only);
            threads.push(spawn("desk-web-accept", move || {
                for conn in listener.incoming() {
                    if hub.stopping() {
                        return;
                    }
                    let s = match conn {
                        Ok(s) => s,
                        Err(e) => {
                            eprintln!("lucina-desk-web: accept on {addr}: {e}");
                            std::thread::sleep(Duration::from_millis(200));
                            continue;
                        }
                    };
                    let Ok(peer) = s.peer_addr() else { continue };
                    if !net::allowed(peer.ip(), lan, only) {
                        eprintln!("lucina-desk-web: refused {}: not allowed here", peer.ip());
                        continue;
                    }
                    // The slot is taken here, before a thread is started: never more than MAX_CONNECTIONS of them.
                    let Some(slot) = take_slot(&active) else {
                        refuse_busy(s);
                        continue;
                    };
                    let (hub, login, assets) = (hub.clone(), login.clone(), assets.clone());
                    let r = spawn("desk-web-conn", move || {
                        let _slot = slot;
                        server::handle(s, hub, login, assets);
                    });
                    if let Err(e) = r {
                        eprintln!("lucina-desk-web: {e}");
                    }
                }
            })?);
        }
        Ok(Server { hub, addrs, threads })
    }

    /// The addresses it listens on, with the ports the system gave when port 0 was asked for.
    pub fn local_addrs(&self) -> &[SocketAddr] {
        &self.addrs
    }

    /// Stops capturing and serving, closes every viewer and lets go of every key and button.
    pub fn stop(self) {
        self.hub.stop.store(true, Ordering::SeqCst);
        self.hub.wake();
        for c in hub::lock(&self.hub.clients).iter() {
            c.close();
        }
        // Wakes each accept loop, which then sees the stop.
        for a in &self.addrs {
            let _ = TcpStream::connect_timeout(a, Duration::from_millis(200));
        }
        for t in self.threads {
            let _ = t.join();
        }
        hub::lock(&self.hub.input).release_all();
    }
}

fn spawn(name: &str, f: impl FnOnce() + Send + 'static) -> Result<JoinHandle<()>, String> {
    std::thread::Builder::new().name(name.into()).spawn(f).map_err(|e| format!("{name}: {e}"))
}

#[cfg(test)]
mod tests;
