//! The connection: rendezvous, relay, login, then one thread that reads, decodes and converts. Input goes out from the
//! screen's thread directly, so it does not wait for a frame being decoded.
//!
//! All protocol is in `lucinadesk-core`, sans-IO; this file only carries its messages.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lucinadesk_core::net::{self, WsReader, WsWriter};
use lucinadesk_core::{Codec, Decoding, Display, Event, Session};

use crate::video::{Av1, Canvas};

/// How long connecting and logging in may take.
const TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Clone)]
pub struct Target {
    pub rendezvous: String,
    pub relay: String,
    pub key: String,
    pub id: String,
    pub password: String,
}

/// A cursor picture from the host.
pub struct CursorPic {
    pub id: u64,
    pub hotx: i32,
    pub hoty: i32,
    pub w: i32,
    pub h: i32,
    pub rgba: Vec<u8>,
}

#[derive(Default)]
pub struct CursorState {
    pub pics: Vec<CursorPic>,
    pub current: Option<u64>,
    pub changed: bool,
}

/// What is counted while a session runs: read by the screen's thread for the log.
#[derive(Default)]
pub struct Counters {
    pub frames: AtomicU64,
    pub bytes: AtomicU64,
    pub decode_us: AtomicU64,
    pub convert_us: AtomicU64,
    pub rows: AtomicU64,
    pub worst_decode_us: AtomicU64,
}

/// The session between the two threads.
pub struct Live {
    pub session: Mutex<Session>,
    pub writer: WsWriter,
    pub canvas: Mutex<Canvas>,
    pub cursor: Mutex<CursorState>,
    pub ended: Mutex<Option<String>>,
    pub counters: Counters,
    pub platform: String,
    pub hostname: String,
    pub display: Display,
    /// Written to when the thread has something for the screen; set by the screen's thread.
    pub wake_fd: std::sync::atomic::AtomicI32,
}

impl Live {
    /// Sends one message made under the lock, in the order of the encryption's counter.
    pub fn send(&self, make: impl FnOnce(&mut Session) -> Option<Vec<u8>>) {
        let Ok(mut s) = self.session.lock() else { return };
        if let Some(bytes) = make(&mut s) {
            if let Err(e) = self.writer.send(&bytes) {
                drop(s);
                self.end(format!("the connection broke: {e}"));
            }
        }
    }

    pub fn end(&self, why: String) {
        if let Ok(mut e) = self.ended.lock() {
            e.get_or_insert(why);
        }
        self.wake();
    }

    pub fn wake(&self) {
        let fd = self.wake_fd.load(Ordering::Relaxed);
        if fd >= 0 {
            let b = 1u8;
            // SAFETY: writes one byte from a live local to the session's non-blocking pipe. A full pipe means the screen is awake.
            unsafe { libc::write(fd, (&b as *const u8).cast(), 1) };
        }
    }

    pub fn hang_up(&self) {
        self.writer.shutdown();
    }
}

fn io(what: &str, e: impl std::fmt::Display) -> String {
    format!("{what}: {e}")
}

/// Connects and logs in. On success a thread keeps the session going and fills the canvas.
pub fn connect(t: &Target, name: &str) -> Result<Arc<Live>, String> {
    // AV1 only. `lucina-app-desk` has one decoder, and a host that cannot send AV1 ends the session with a clear message.
    let decoding = Decoding { av1: true, prefer: 5, custom_fps: 30, ..Decoding::default() };
    let mut s = Session::new(&t.id, &t.key, &t.password, decoding, name)?;
    s.set_platform("Linux");

    let (mut rv, rv_w) = net::connect(&t.rendezvous, TIMEOUT).map_err(|e| io("no answer from the rendezvous server", e))?;
    rv_w.send(&s.rendezvous_open()).map_err(|e| io("rendezvous", e))?;
    'rendezvous: loop {
        let Some(m) = rv.read().map_err(|e| io("rendezvous", e))? else {
            return Err("the rendezvous server closed the connection".into());
        };
        let out = s.rendezvous_recv(&m);
        for b in &out.send {
            rv_w.send(b).map_err(|e| io("rendezvous", e))?;
        }
        for e in out.events {
            match e {
                Event::Relay { .. } => break 'rendezvous,
                Event::Closed { reason } => return Err(reason),
                _ => {}
            }
        }
    }
    rv_w.shutdown();

    let (mut relay, writer) = net::connect(&t.relay, TIMEOUT).map_err(|e| io("no answer from the relay", e))?;
    writer.send(&s.relay_open()).map_err(|e| io("relay", e))?;
    let started = Instant::now();
    // The first frame can come in the same read as the login's answer: it waits here.
    let mut early: Vec<Event> = Vec::new();
    let (platform, hostname, display) = 'login: loop {
        if started.elapsed() > TIMEOUT {
            return Err("the host did not answer in time".into());
        }
        let Some(m) = relay.read().map_err(|e| io("relay", e))? else {
            return Err("the relay closed the connection: is the host online?".into());
        };
        let out = s.relay_recv(&m);
        for b in &out.send {
            writer.send(b).map_err(|e| io("relay", e))?;
        }
        let mut found = None;
        for e in out.events {
            match e {
                Event::LoggedIn { platform, hostname, displays, current_display, .. } => {
                    let i = current_display.max(0) as usize;
                    let d = displays.into_iter().nth(i).ok_or("the host has no display")?;
                    found = Some((platform, hostname, d));
                }
                Event::Closed { reason } => return Err(reason),
                other if found.is_some() => early.push(other),
                _ => {}
            }
        }
        if let Some(f) = found {
            break 'login f;
        }
    };
    eprintln!("lucina-app-desk: logged in to {} ({}), {}x{}", hostname, platform, display.width, display.height);

    let live = Arc::new(Live {
        session: Mutex::new(s),
        writer,
        canvas: Mutex::new(Canvas::default()),
        cursor: Mutex::new(CursorState::default()),
        ended: Mutex::new(None),
        counters: Counters::default(),
        platform,
        hostname,
        display,
        wake_fd: std::sync::atomic::AtomicI32::new(-1),
    });
    let l = live.clone();
    std::thread::spawn(move || {
        let why = reader_thread(&l, relay, early);
        l.end(why);
    });
    Ok(live)
}

fn add(c: &AtomicU64, v: u64) {
    c.fetch_add(v, Ordering::Relaxed);
}

/// Reads until the session ends and says why it did.
fn reader_thread(live: &Live, mut relay: WsReader, early: Vec<Event>) -> String {
    let mut dec = match Av1::new() {
        Ok(d) => d,
        Err(e) => return e,
    };
    for e in early {
        if let Some(why) = handle(live, &mut dec, e) {
            return why;
        }
    }
    loop {
        let m = match relay.read() {
            Ok(Some(m)) => m,
            Ok(None) => return "the host closed the connection".into(),
            Err(e) => return format!("the connection broke: {e}"),
        };
        let out = match live.session.lock() {
            Ok(mut s) => s.relay_recv(&m),
            Err(_) => return "a thread failed".into(),
        };
        for b in &out.send {
            if let Err(e) = live.writer.send(b) {
                return format!("the connection broke: {e}");
            }
        }
        for e in out.events {
            if let Some(why) = handle(live, &mut dec, e) {
                return why;
            }
        }
    }
}

fn handle(live: &Live, dec: &mut Av1, e: Event) -> Option<String> {
    match e {
        Event::Video { codec, frames, .. } => {
            if codec != Codec::Av1 {
                return Some(format!("the host sends {} and this app decodes AV1 only", codec.name()));
            }
            let c = &live.counters;
            for f in &frames {
                let t0 = Instant::now();
                let pic = match dec.decode(&f.data) {
                    Ok(p) => p,
                    Err(err) => {
                        eprintln!("lucina-app-desk: {err}; asking for a key frame");
                        live.send(|s| s.refresh_video());
                        continue;
                    }
                };
                let t1 = Instant::now();
                add(&c.frames, 1);
                add(&c.bytes, f.data.len() as u64);
                let dus = (t1 - t0).as_micros() as u64;
                add(&c.decode_us, dus);
                c.worst_decode_us.fetch_max(dus, Ordering::Relaxed);
                if let Some(p) = pic {
                    let rows = match live.canvas.lock() {
                        Ok(mut canvas) => canvas.update(&p),
                        Err(_) => return Some("a thread failed".into()),
                    };
                    add(&c.convert_us, t1.elapsed().as_micros() as u64);
                    add(&c.rows, rows as u64);
                    if rows > 0 {
                        live.wake();
                    }
                }
            }
            live.send(|s| s.video_ack());
        }
        Event::CursorData { id, hotx, hoty, width, height, rgba } => {
            if let Ok(mut cur) = live.cursor.lock() {
                cur.pics.retain(|p| p.id != id);
                // Hosts reuse a few dozen pictures; keep it bounded against one that does not.
                if cur.pics.len() >= 64 {
                    cur.pics.remove(0);
                }
                cur.pics.push(CursorPic { id, hotx, hoty, w: width, h: height, rgba });
                cur.current = Some(id);
                cur.changed = true;
            }
            live.wake();
        }
        Event::CursorId { id } => {
            if let Ok(mut cur) = live.cursor.lock() {
                cur.current = Some(id);
                cur.changed = true;
            }
            live.wake();
        }
        Event::Closed { reason } => return Some(reason),
        _ => {}
    }
    None
}
