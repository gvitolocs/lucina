//! The other direction: another device reaches this laptop. Off unless it is asked for. Three ways in:
//!
//! - `--host --direct` (`direct.rs`): a client on this network dials this laptop's address. What the app's switch starts.
//! - `--host --web` (`web.rs`): a browser on this network opens this laptop's address; lucina-desk-web speaks voxl's protocol.
//! - `--host`: no port listens. The process registers with hbbs over UDP and goes out to hbbr when a client with the server's key
//!   asks, and the password is checked here, with only a hash of it stored. `--host-setup` stores those settings, password on stdin.
//!
//! Both run until SIGTERM.

pub mod capture;
pub mod check;
pub mod config;
pub mod control;
pub mod direct;
pub mod inject;
pub mod web;

use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lucina_vp8::{Encoder, Yuv};
use lucinadesk_core::host::{self, HostEvent, HostInfo, HostSession, Identity, RendezvousEvent};
use lucinadesk_core::net;

use capture::{Capture, Source};
use config::HostConfig;
use inject::Injector;

const TIMEOUT: Duration = Duration::from_secs(15);
/// Frames out without an ack before the host waits: the client's decoder sets the pace.
const MAX_UNACKED: u32 = 3;
const DEFAULT_FPS: u32 = 15;
/// The most a client can ask for: one CPU also runs what is being looked at.
const MOST_FPS: u32 = 30;
/// This share of each round is left to everything else, at the least.
const IDLE_SHARE_PERCENT: u64 = 25;
/// Wrong passwords before the host stops listening to anyone for a while, and how long.
const MAX_FAILURES: u32 = 3;
const LOCKOUT: Duration = Duration::from_secs(60);

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_stop(_: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

fn stopping() -> bool {
    STOP.load(Ordering::SeqCst)
}

fn install_signals() {
    // SAFETY: the handler only stores to an atomic.
    unsafe {
        libc::signal(libc::SIGTERM, on_stop as extern "C" fn(libc::c_int) as libc::sighandler_t);
        libc::signal(libc::SIGINT, on_stop as extern "C" fn(libc::c_int) as libc::sighandler_t);
        // What the person is doing on this machine goes first.
        libc::setpriority(libc::PRIO_PROCESS, 0, 5);
    }
}

/// Quantizer by how much of the picture changed: little change, sharp; the whole screen, coarse but on time.
pub fn pick_q(changed: usize, total: usize) -> u8 {
    match changed * 100 / total.max(1) {
        0..=4 => 14,
        5..=24 => 26,
        25..=59 => 44,
        _ => 60,
    }
}
const BEST_Q: u8 = 14;
const KEY_Q: u8 = 40;
/// Macroblocks sharpened in a frame with nothing else to do.
const REFINE_BATCH: usize = 500;

struct Shared {
    session: Mutex<HostSession>,
    writer: net::Writer,
    closed: AtomicBool,
    unacked: AtomicU32,
    want_key: AtomicBool,
    fps: AtomicU32,
    logged_in: AtomicBool,
    wants_acks: AtomicBool,
    failures: AtomicI32,
}

impl Shared {
    fn send(&self, b: &[u8]) -> bool {
        if self.writer.send(b).is_err() {
            self.closed.store(true, Ordering::SeqCst);
            return false;
        }
        true
    }
}

/// The session's input, from the relay: until it closes.
fn reader(shared: Arc<Shared>, mut relay: net::Reader, mut injector: Injector) {
    loop {
        let m = match relay.read() {
            Ok(Some(m)) => m,
            Ok(None) => {
                eprintln!("lucina-app-desk: host: the client left");
                break;
            }
            Err(e) => {
                eprintln!("lucina-app-desk: host: connection: {e}");
                break;
            }
        };
        let out = match shared.session.lock() {
            Ok(mut s) => s.recv(&m),
            Err(_) => break,
        };
        for b in &out.send {
            shared.send(b);
        }
        let mut done = false;
        for e in out.events {
            match e {
                HostEvent::LoggedIn { name, platform, wants_acks } => {
                    eprintln!("lucina-app-desk: host: {name} ({platform}) logged in, acks {wants_acks}");
                    shared.wants_acks.store(wants_acks, Ordering::SeqCst);
                    shared.want_key.store(true, Ordering::SeqCst);
                    shared.logged_in.store(true, Ordering::SeqCst);
                }
                HostEvent::LoginFailed { name } => {
                    eprintln!("lucina-app-desk: host: wrong password from {name}");
                    shared.failures.fetch_add(1, Ordering::SeqCst);
                }
                HostEvent::Mouse { mask, x, y } => injector.mouse(mask, x, y),
                HostEvent::Key(k) => injector.key(k),
                HostEvent::VideoAck => {
                    let _ = shared.unacked.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |v| Some(v.saturating_sub(1)));
                }
                HostEvent::RefreshVideo => shared.want_key.store(true, Ordering::SeqCst),
                HostEvent::Options { custom_fps, .. } => {
                    if custom_fps > 0 {
                        shared.fps.store((custom_fps as u32).clamp(1, MOST_FPS), Ordering::SeqCst);
                    }
                }
                HostEvent::Closed { reason } => {
                    eprintln!("lucina-app-desk: host: session over: {reason}");
                    done = true;
                }
            }
        }
        if done {
            break;
        }
    }
    injector.release_all();
    shared.closed.store(true, Ordering::SeqCst);
    shared.writer.shutdown();
}

/// Numbers for the log every ten seconds, what `docs/research/juno-desk.md` quotes.
#[derive(Default)]
struct Stats {
    frames: u64,
    keys: u64,
    bytes: u64,
    examined: u64,
    capture: Duration,
    convert: Duration,
    encode: Duration,
    worst_round: Duration,
}

fn process_cpu() -> Duration {
    let mut t = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: fills a live timespec.
    unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut t) };
    Duration::new(t.tv_sec as u64, t.tv_nsec as u32)
}

/// The session's video, until the client leaves.
fn video(shared: &Shared, source: &mut dyn Source) -> Result<(), String> {
    // Nothing is captured until the password was right.
    let start = Instant::now();
    while !shared.logged_in.load(Ordering::SeqCst) {
        if shared.closed.load(Ordering::SeqCst) || stopping() {
            return Ok(());
        }
        if start.elapsed() > Duration::from_secs(60) {
            return Err("nobody logged in".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let (w, h) = source.size();
    let mut cap = Capture::new(w, h);
    let mut enc = Encoder::new(w, h);
    let (mw, mh) = enc.macroblocks();
    let total = mw * mh;
    let mut out = Vec::with_capacity(1 << 20);
    let t0 = Instant::now();
    let mut last_probe = Instant::now();
    let mut last_frame = Instant::now();
    let mut stats = Stats::default();
    let (mut report_at, mut cpu_at) = (Instant::now(), process_cpu());
    loop {
        if shared.closed.load(Ordering::SeqCst) || stopping() {
            return Ok(());
        }
        let round = Instant::now();
        let want_key = shared.want_key.load(Ordering::SeqCst);
        if !shared.wants_acks.load(Ordering::SeqCst) || shared.unacked.load(Ordering::SeqCst) < MAX_UNACKED || want_key || last_frame.elapsed() > Duration::from_secs(2) {
            let t1 = Instant::now();
            let changed = cap.scan(source, want_key);
            let t2 = Instant::now();
            cap.convert();
            let t3 = Instant::now();
            stats.capture += t2 - t1;
            stats.convert += t3 - t2;
            let refine = if changed == 0 { enc.coarser_than(BEST_Q) } else { 0 };
            // A frame now and then even when nothing moved: quiet clients treat a long silence as a dead host.
            let keep_alive = last_frame.elapsed() > Duration::from_secs(1);
            if changed > 0 || want_key || refine > 0 || keep_alive {
                shared.want_key.store(false, Ordering::SeqCst);
                let q = if want_key {
                    KEY_Q
                } else if changed == 0 {
                    BEST_Q
                } else {
                    pick_q(changed, total)
                };
                let (y, u, v) = cap.planes();
                let pic = Yuv { y, u, v, y_stride: w, uv_stride: w.div_ceil(2) };
                let info = enc.encode(&pic, cap.changed(), q, want_key, if changed == 0 { REFINE_BATCH } else { 0 }, &mut out);
                let t4 = Instant::now();
                stats.encode += t4 - t3;
                stats.frames += 1;
                stats.keys += info.key as u64;
                stats.bytes += out.len() as u64;
                stats.examined += info.examined as u64;
                let frame = match shared.session.lock() {
                    Ok(mut s) => s.video_vp8(out.clone(), info.key, t0.elapsed().as_millis() as i64),
                    Err(_) => return Err("a thread failed".into()),
                };
                if let Some(b) = frame {
                    if !shared.send(&b) {
                        return Ok(());
                    }
                    shared.unacked.fetch_add(1, Ordering::SeqCst);
                }
                last_frame = Instant::now();
            }
        }
        if last_probe.elapsed() > Duration::from_secs(3) {
            last_probe = Instant::now();
            let b = shared.session.lock().ok().and_then(|mut s| s.test_delay(t0.elapsed().as_millis() as i64));
            if let Some(b) = b {
                shared.send(&b);
            }
        }
        let took = round.elapsed();
        stats.worst_round = stats.worst_round.max(took);
        if report_at.elapsed() >= Duration::from_secs(10) {
            let dt = report_at.elapsed().as_secs_f64();
            let n = stats.frames.max(1) as f64;
            let cpu = process_cpu();
            eprintln!(
                "lucina-app-desk: host: stats {:.1} fps ({} key), {:.0} kB/s, {:.0} blocks a frame, capture {:.1} ms, convert {:.1}, encode {:.1} ms a frame, worst round {:.0} ms, CPU {:.0}%",
                stats.frames as f64 / dt,
                stats.keys,
                stats.bytes as f64 / 1000.0 / dt,
                stats.examined as f64 / n,
                stats.capture.as_secs_f64() * 1000.0 / n,
                stats.convert.as_secs_f64() * 1000.0 / n,
                stats.encode.as_secs_f64() * 1000.0 / n,
                stats.worst_round.as_secs_f64() * 1000.0,
                (cpu - cpu_at).as_secs_f64() * 100.0 / dt,
            );
            stats = Stats::default();
            report_at = Instant::now();
            cpu_at = cpu;
        }
        // One round's pace, and never all of the CPU: this machine has one, and it also runs what the client is watching.
        let tick = Duration::from_millis(1000 / shared.fps.load(Ordering::SeqCst).max(1) as u64);
        let rest = tick.saturating_sub(took).max(took * IDLE_SHARE_PERCENT as u32 / (100 - IDLE_SHARE_PERCENT) as u32);
        std::thread::sleep(rest);
    }
}

/// One incoming session: tells hbbs which relay session to use, joins it, serves it. Returns the number of wrong passwords.
fn session(cfg: &HostConfig, identity: &Identity, socket_addr: Vec<u8>, source: &mut dyn Source) -> Result<i32, String> {
    let uuid = host::new_relay_uuid();
    let (mut hbbs_r, hbbs) = net::connect(&cfg.rendezvous_url(), TIMEOUT).map_err(|e| format!("hbbs: {e}"))?;
    hbbs.send(&host::relay_response(&identity.id, socket_addr, &uuid, &cfg.client_relay)).map_err(|e| format!("hbbs: {e}"))?;
    // The relay comes first in time: the client dials it as soon as hbbs passes this on.
    let (relay_r, relay) = net::connect(&cfg.relay_url(), TIMEOUT).map_err(|e| format!("hbbr: {e}"))?;
    relay.send(&host::request_relay(&uuid, &cfg.key)).map_err(|e| format!("hbbr: {e}"))?;
    let hbbs_done = hbbs.clone();
    std::thread::spawn(move || {
        // hbbs closes this when it is done; nothing is said on it.
        let _ = hbbs_r.read();
        hbbs_done.shutdown();
    });

    let s = HostSession::new(identity, cfg.password.clone(), host_info(source));
    let res = serve(s, net::Reader::Ws(relay_r), net::Writer::Ws(relay), source);
    hbbs.shutdown();
    res
}

/// What a client is told about this machine.
fn host_info(source: &dyn Source) -> HostInfo {
    let (w, h) = source.size();
    HostInfo {
        hostname: std::fs::read_to_string("/etc/hostname").map(|s| s.trim().to_owned()).ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "juno".into()),
        username: "owner".into(),
        platform: "Linux".into(),
        width: w as i32,
        height: h as i32,
    }
}

/// One connection, whichever way it came: until the client leaves. Returns the number of wrong passwords.
fn serve(mut s: HostSession, from: net::Reader, to: net::Writer, source: &mut dyn Source) -> Result<i32, String> {
    let first = s.start();
    to.send(&first).map_err(|e| format!("the client: {e}"))?;
    let shared = Arc::new(Shared {
        session: Mutex::new(s),
        writer: to.clone(),
        closed: AtomicBool::new(false),
        unacked: AtomicU32::new(0),
        want_key: AtomicBool::new(true),
        fps: AtomicU32::new(DEFAULT_FPS),
        logged_in: AtomicBool::new(false),
        wants_acks: AtomicBool::new(false),
        failures: AtomicI32::new(0),
    });
    let injector = if source.is_test() { Injector::new(Box::new(inject::LogSink::default())) } else { Injector::open()? };
    let r = shared.clone();
    let t = std::thread::spawn(move || reader(r, from, injector));
    let res = video(&shared, source);
    if let Ok(mut s) = shared.session.lock() {
        if let Some(b) = s.goodbye("The host stopped") {
            let _ = to.send(&b);
        }
    }
    to.shutdown();
    let _ = t.join();
    res?;
    Ok(shared.failures.load(Ordering::SeqCst))
}

/// Runs until SIGTERM.
pub fn run(cfg: HostConfig, mut source: Box<dyn Source>) -> Result<(), String> {
    install_signals();
    let identity = cfg.identity();
    let udp = UdpSocket::bind("0.0.0.0:0").map_err(|e| e.to_string())?;
    udp.connect(&cfg.udp).map_err(|e| format!("{}: {e}", cfg.udp))?;
    udp.set_read_timeout(Some(Duration::from_secs(1))).map_err(|e| e.to_string())?;
    eprintln!("lucina-app-desk: host: {} through {}, no port listening", identity.id, cfg.udp);
    let _ = udp.send(&host::register_pk(&identity));
    let mut last = Instant::now();
    let mut buf = vec![0u8; 4096];
    let mut failures: Vec<Instant> = Vec::new();
    let mut registered = false;
    while !stopping() {
        if last.elapsed() >= Duration::from_secs(host::REGISTER_EVERY_SECS) {
            last = Instant::now();
            // A failure here is a network that comes and goes: the next round tries again.
            let _ = udp.send(&host::register_peer(&identity.id));
        }
        let n = match udp.recv(&mut buf) {
            Ok(n) => n,
            Err(_) => continue,
        };
        match host::parse_rendezvous(&buf[..n]) {
            RendezvousEvent::WantsKey => {
                let _ = udp.send(&host::register_pk(&identity));
            }
            RendezvousEvent::Registered(Ok(())) => {
                if !registered {
                    eprintln!("lucina-app-desk: host: registered");
                    registered = true;
                }
            }
            RendezvousEvent::Registered(Err(e)) => eprintln!("lucina-app-desk: host: hbbs refused: {e}"),
            RendezvousEvent::Incoming { socket_addr } => {
                failures.retain(|t| t.elapsed() < LOCKOUT);
                if failures.len() as u32 >= MAX_FAILURES {
                    eprintln!("lucina-app-desk: host: {MAX_FAILURES} wrong passwords: not answering for a while");
                    continue;
                }
                match session(&cfg, &identity, socket_addr, source.as_mut()) {
                    Ok(wrong) => {
                        for _ in 0..wrong {
                            failures.push(Instant::now());
                        }
                    }
                    Err(e) => eprintln!("lucina-app-desk: host: {e}"),
                }
                // Hbbs forgets a host that was quiet for the session's length.
                let _ = udp.send(&host::register_peer(&identity.id));
                last = Instant::now();
            }
            RendezvousEvent::Other => {}
        }
    }
    eprintln!("lucina-app-desk: host: stopped");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_quantizer_follows_the_share_that_changed() {
        assert_eq!(pick_q(1, 100), BEST_Q);
        assert!(pick_q(10, 100) > pick_q(1, 100));
        assert!(pick_q(50, 100) > pick_q(10, 100));
        assert!(pick_q(100, 100) > pick_q(50, 100));
        assert_eq!(pick_q(0, 0), BEST_Q);
    }

}
