//! The controller a screen uses: the socket ([`crate::link`]), the turn ([`crate::session`]), a microphone thread that decimates
//! and sends 40 ms frames while a turn streams, and a player thread for the reply's speech. The microphone and the speakers are
//! traits, so the Juno app, the host check and the tests each bring their own.
//!
//! Locking: one mutex holds the session, and every action it returns is carried out under it, so frames and the held audio cannot
//! be reordered between the microphone thread and the socket thread. `on_event` therefore runs under that lock: it must only queue
//! the event (and wake the screen), never call back into [`Voice`].
//!
//! Nothing here logs audio, text or the token. Timings are logged as numbers on stderr.
use std::io;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{mpsc, Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::dsp::{Down48to16, Framer, Up24to48};
use crate::link::{Incoming, Link, LinkConfig, Outgoing};
use crate::msg::{self, Start};
use crate::rules;
use crate::session::{Action, Busy, Event, Session};

/// 40 ms of the microphone's 48 kHz mono s16le: three times the 1280-byte frame of 16 kHz.
const MIC_READ: usize = 3840;

/// A microphone: 48 kHz mono s16le; `read` blocks until some samples are there, and returns 0 at the end.
pub trait PcmSource: Send {
    fn open(&mut self) -> io::Result<()>;
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize>;
    fn close(&mut self);
}

/// The speakers: 48 kHz stereo s16le. `write` takes what fits without blocking (`WouldBlock` when nothing does).
pub trait PcmSink: Send {
    fn open(&mut self) -> io::Result<()>;
    /// Lets the device go; what is queued plays out unless [`PcmSink::flush`] went first.
    fn close(&mut self);
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize>;
    fn flush(&mut self);
    fn set_volume(&mut self, db: i32);
}

pub type SourceFactory = Box<dyn Fn() -> Box<dyn PcmSource> + Send + Sync>;

pub struct VoiceConfig {
    pub link: LinkConfig,
    pub source: SourceFactory,
    pub sink: Box<dyn PcmSink>,
    /// The speakers' volume, set after the sink opens (the kernel starts a stream at -30 dB).
    pub volume_db: i32,
    /// Called for every [`Event`], under the controller's lock.
    pub on_event: Arc<dyn Fn(Event) + Send + Sync>,
    /// Logs timings to stderr.
    pub log_timings: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PressError {
    /// No socket yet (it reconnects by itself).
    NotConnected,
    /// A turn is open, or the last microphone is still closing.
    Busy,
}

enum Play {
    Data(Vec<u8>),
    End,
    Flush,
}

struct Inner {
    session: Session,
    /// A microphone thread is running (or finishing).
    capturing: bool,
    /// The current turn's stop flag: 0 running, 1 end, 2 cancel.
    stop: Arc<AtomicU8>,
}

struct Shared {
    inner: Mutex<Inner>,
    link: Mutex<Option<Link>>,
    play: mpsc::Sender<Play>,
    on_event: Arc<dyn Fn(Event) + Send + Sync>,
    log_timings: bool,
    flush_gen: Arc<AtomicU64>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl Shared {
    /// Carries out what the session decided. Called with the session lock held.
    fn run(&self, actions: Vec<Action>) {
        for a in actions {
            match a {
                Action::Text(t) => self.send(Outgoing::Text(t)),
                Action::Binary(b) => self.send(Outgoing::Binary(b)),
                Action::Play(b) => {
                    let _ = self.play.send(Play::Data(b));
                }
                Action::PlayEnd => {
                    let _ = self.play.send(Play::End);
                }
                Action::PlayFlush => {
                    self.flush_gen.fetch_add(1, Ordering::AcqRel);
                    let _ = self.play.send(Play::Flush);
                }
                Action::Event(e) => {
                    if let Event::Timing { what, ms } = &e {
                        if self.log_timings {
                            eprintln!("voice: {what} {ms} ms");
                        }
                    }
                    (self.on_event)(e);
                }
            }
        }
    }

    fn send(&self, m: Outgoing) {
        if let Some(l) = lock(&self.link).as_ref() {
            let _ = l.send(m);
        }
    }

}

pub struct Voice {
    shared: Arc<Shared>,
    source: Arc<SourceFactory>,
}

impl Voice {
    /// Opens the socket (it connects in the background and stays) and starts the player.
    pub fn start(cfg: VoiceConfig) -> Voice {
        let (play_tx, play_rx) = mpsc::channel();
        let flush_gen = Arc::new(AtomicU64::new(0));
        let shared = Arc::new(Shared {
            inner: Mutex::new(Inner { session: Session::new(), capturing: false, stop: Arc::new(AtomicU8::new(0)) }),
            link: Mutex::new(None),
            play: play_tx,
            on_event: cfg.on_event.clone(),
            log_timings: cfg.log_timings,
            flush_gen: flush_gen.clone(),
        });
        let sink = cfg.sink;
        let volume = cfg.volume_db;
        let _ = std::thread::Builder::new().name("voice-player".into()).spawn(move || player_thread(sink, volume, play_rx, flush_gen));
        let weak = Arc::downgrade(&shared);
        let handler: Arc<dyn Fn(Incoming) + Send + Sync> = Arc::new(move |ev| {
            if let Some(shared) = weak.upgrade() {
                on_link(&shared, ev);
            }
        });
        let link = Link::spawn(cfg.link, handler);
        *lock(&shared.link) = Some(link);
        Voice { shared, source: Arc::new(cfg.source) }
    }

    pub fn connected(&self) -> bool {
        lock(&self.shared.link).as_ref().is_some_and(Link::is_up)
    }

    /// The phase of the open turn (`rules::IDLE` ...).
    pub fn phase(&self) -> u8 {
        lock(&self.shared.inner).session.phase()
    }

    /// Passes of the socket loop so far; an idle link adds none (tests).
    pub fn link_passes(&self) -> u64 {
        lock(&self.shared.link).as_ref().map_or(0, Link::passes)
    }

    pub fn pending_notes(&self) -> usize {
        lock(&self.shared.inner).session.pending_notes()
    }

    /// Skips the rest of a reconnect backoff.
    pub fn retry_now(&self) {
        if let Some(l) = lock(&self.shared.link).as_ref() {
            l.retry_now();
        }
    }

    /// The owner pressed: `start` goes out now, and the microphone's first samples are held until the server listens.
    pub fn press(&self, start: Start) -> Result<(), PressError> {
        let stop;
        {
            let mut inner = lock(&self.shared.inner);
            if inner.capturing {
                return Err(PressError::Busy);
            }
            if !lock(&self.shared.link).as_ref().is_some_and(Link::is_up) {
                return Err(PressError::NotConnected);
            }
            let actions = inner.session.press(&start).map_err(|Busy| PressError::Busy)?;
            stop = Arc::new(AtomicU8::new(0));
            inner.stop = stop.clone();
            inner.capturing = true;
            self.shared.run(actions);
        }
        let shared = self.shared.clone();
        let source = self.source.clone();
        let spawned = std::thread::Builder::new().name("voice-mic".into()).spawn({
            let shared = shared.clone();
            move || capture_thread(shared, source, stop)
        });
        if spawned.is_err() {
            let mut inner = lock(&shared.inner);
            inner.capturing = false;
            let actions = inner.session.cancel();
            shared.run(actions);
        }
        Ok(())
    }

    /// The owner let go: the last audio goes, then `end`.
    pub fn end(&self) {
        lock(&self.shared.inner).stop.store(1, Ordering::Release);
    }

    /// Esc: drop the turn (nothing is stored for a note that has not been stored).
    pub fn cancel(&self) {
        let mut inner = lock(&self.shared.inner);
        inner.stop.store(2, Ordering::Release);
        let actions = inner.session.cancel();
        self.shared.run(actions);
    }
}

impl Drop for Voice {
    fn drop(&mut self) {
        {
            let mut inner = lock(&self.shared.inner);
            inner.stop.store(2, Ordering::Release);
            let actions = inner.session.cancel();
            self.shared.run(actions);
        }
        // Closes the socket and joins its thread; the thread's handler holds only a weak reference to `shared`. The player ends
        // when `shared` (which holds its channel) is dropped, after the last microphone thread has let go of it.
        let link = lock(&self.shared.link).take();
        drop(link);
    }
}

fn on_link(shared: &Arc<Shared>, ev: Incoming) {
    let mut inner = lock(&shared.inner);
    let now = Instant::now();
    match ev {
        Incoming::Up => (shared.on_event)(Event::Connected),
        Incoming::Down(reason) => {
            let actions = inner.session.closed();
            shared.run(actions);
            (shared.on_event)(Event::Disconnected(reason));
        }
        Incoming::Text(t) => {
            if let Some(m) = msg::parse(&t) {
                let actions = inner.session.server(m, now);
                shared.run(actions);
            }
        }
        Incoming::Binary(b) => {
            let actions = inner.session.binary(b, now);
            shared.run(actions);
        }
    }
    if inner.session.phase() == rules::IDLE {
    }
}

fn capture_thread(shared: Arc<Shared>, source: Arc<SourceFactory>, stop: Arc<AtomicU8>) {
    let finish = |end: bool, rest: Vec<u8>| {
        let mut inner = lock(&shared.inner);
        if end && Arc::ptr_eq(&inner.stop, &stop) {
            let a = inner.session.frame(rest);
            shared.run(a);
            let a = inner.session.end(Instant::now());
            shared.run(a);
        }
        inner.capturing = false;
        if inner.session.phase() == rules::IDLE {
        }
    };
    let mut mic = source();
    if mic.open().is_err() {
        let mut inner = lock(&shared.inner);
        let a = inner.session.cancel();
        shared.run(a);
        (shared.on_event)(Event::Failed { code: "mic".into(), message: "The microphone is not available.".into() });
        inner.capturing = false;
        return;
    }
    let mut down = Down48to16::new();
    let mut framer = Framer::new();
    let mut buf = vec![0u8; MIC_READ];
    let mut samples = Vec::with_capacity(MIC_READ / 6 + 8);
    let mut end = false;
    loop {
        if stop.load(Ordering::Acquire) == 2 {
            break;
        }
        let n = match mic.read(&mut buf) {
            Ok(0) => {
                // The source is over (a file): the same as letting go.
                end = true;
                break;
            }
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => {
                let mut inner = lock(&shared.inner);
                let a = inner.session.cancel();
                shared.run(a);
                (shared.on_event)(Event::Failed { code: "mic".into(), message: "The microphone stopped.".into() });
                break;
            }
        };
        samples.clear();
        down.push_bytes(&buf[..n], &mut samples);
        let mut frames = Vec::new();
        framer.push(&samples, |f| frames.push(f));
        {
            let mut inner = lock(&shared.inner);
            for f in frames {
                let a = inner.session.frame(f);
                shared.run(a);
            }
            if !inner.session.wants_audio() {
                // The server's endpoint, an error or a closed socket: no more audio for this turn.
                break;
            }
        }
        if stop.load(Ordering::Acquire) == 1 {
            end = true;
            break;
        }
    }
    mic.close();
    let rest = if end { framer.finish() } else { Vec::new() };
    finish(end, rest);
}

fn player_thread(mut sink: Box<dyn PcmSink>, volume_db: i32, rx: mpsc::Receiver<Play>, flush_gen: Arc<AtomicU64>) {
    let mut up = Up24to48::new();
    let mut opened = false;
    let mut wide = Vec::new();
    while let Ok(m) = rx.recv() {
        match m {
            Play::Data(bytes) => {
                if !opened {
                    if sink.open().is_err() {
                        continue;
                    }
                    sink.set_volume(volume_db);
                    opened = true;
                }
                wide.clear();
                up.push_bytes(&bytes, &mut wide);
                let epoch = flush_gen.load(Ordering::Acquire);
                let mut at = 0;
                let mut stalled = Instant::now();
                while at < wide.len() && flush_gen.load(Ordering::Acquire) == epoch {
                    match sink.write(&wide[at..]) {
                        Ok(0) => std::thread::sleep(Duration::from_millis(5)),
                        Ok(n) => {
                            at += n;
                            stalled = Instant::now();
                        }
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                            if stalled.elapsed() > Duration::from_secs(10) {
                                break;
                            }
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(_) => break,
                    }
                }
            }
            Play::End => {
                if opened {
                    sink.close();
                    opened = false;
                }
                up = Up24to48::new();
            }
            Play::Flush => {
                if opened {
                    sink.flush();
                    sink.close();
                    opened = false;
                }
                up = Up24to48::new();
            }
        }
    }
    if opened {
        sink.close();
    }
}
