//! What the capture thread and the connections share, as voxl's `hub.rs`, on std's channels, mutexes and condvars.

use std::collections::VecDeque;
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::input::Keys;
use crate::wait::Activity;

/// Locks, and goes on with the data if another thread panicked holding it: nothing here is left half-changed by a panic
/// that matters more than the host staying up.
pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// What a connection's writer sends.
pub enum Out {
    Text(String),
    /// A video message, shared by every viewer it goes to.
    Bin(Arc<Vec<u8>>),
    Pong(Vec<u8>),
}

pub struct Client {
    pub id: u64,
    tx: Mutex<Sender<Out>>,
    /// For shutting the connection down from outside (a stop, a hopeless backlog).
    stream: TcpStream,
    /// `v,1`: wants the picture.
    pub video: AtomicBool,
    /// Has had a keyframe of every stripe since it (re)started; until then it is sent only keyframes.
    pub started: AtomicBool,
    pub needs_keyframe: AtomicBool,
    /// Frames sent and not acknowledged: (id, when sent).
    pub inflight: Mutex<VecDeque<(u32, Instant)>>,
    /// Smallest RTT of the last 20 pings, ms (0: none yet).
    pub min_rtt_ms: AtomicU32,
}

impl Client {
    pub fn send(&self, m: Out) {
        let _ = lock(&self.tx).send(m);
    }

    /// `a,<id>`: that frame and every one before it arrived.
    pub fn on_ack(&self, id: u32) {
        let mut q = lock(&self.inflight);
        while q.front().is_some_and(|f| wrapping_le(f.0, id)) {
            q.pop_front();
        }
    }

    /// Frames in flight and the age of the oldest.
    pub fn backlog(&self) -> (usize, Duration) {
        let q = lock(&self.inflight);
        (q.len(), q.front().map_or(Duration::ZERO, |f| f.1.elapsed()))
    }

    pub fn close(&self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }
}

fn wrapping_le(a: u32, b: u32) -> bool {
    b.wrapping_sub(a) < 0x8000_0000
}

/// voxl's `/stats` counters, the ones that mean something here.
#[derive(Default)]
pub struct Stats {
    /// Rounds that found something changed.
    pub captured: AtomicU64,
    /// Frames sent (a frame: the stripes of one round).
    pub encoded: AtomicU64,
    pub bytes: AtomicU64,
    pub encode_us: AtomicU64,
    /// Frames with a keyframe of every stripe.
    pub keyframes: AtomicU64,
    pub paintovers: AtomicU64,
    pub flow_blocked: AtomicU64,
}

/// The screen as captured, for a screenshot.
pub struct Snapshot {
    pub width: usize,
    pub height: usize,
    pub xrgb: Vec<u32>,
    /// The latest change (`/wait`) the pixels include.
    pub seq: u64,
}

/// Wakes the capture thread: acks, joins, keyframe requests, screenshots, waits, the stop.
#[derive(Default)]
pub struct Wake {
    signalled: Mutex<bool>,
    cv: Condvar,
}

impl Wake {
    pub fn signal(&self) {
        *lock(&self.signalled) = true;
        self.cv.notify_all();
    }

    /// Until signalled or `timeout`; either way the signal is used up.
    pub fn wait(&self, timeout: Duration) {
        let mut s = lock(&self.signalled);
        if !*s {
            s = self.cv.wait_timeout(s, timeout).unwrap_or_else(|e| e.into_inner()).0;
        }
        *s = false;
    }
}

pub struct Hub {
    pub clients: Mutex<Vec<Arc<Client>>>,
    next_id: AtomicU64,
    pub stats: Stats,
    pub width: u32,
    pub height: u32,
    /// Encoded stripes as (y, height).
    pub stripes: Vec<(u32, u32)>,
    pub read_only: bool,
    pub epoch: Instant,
    pub waker: Wake,
    pub stop: AtomicBool,
    pub input: Mutex<Keys>,
    /// Screenshot requests, answered by the capture thread.
    pub snapshots: Mutex<Vec<Sender<Arc<Snapshot>>>>,
    /// `/wait` requests open: the capture thread scans for them even with no viewer.
    pub waiters: AtomicUsize,
    pub activity: Activity,
}

impl Hub {
    pub fn new(width: u32, height: u32, stripes: Vec<(u32, u32)>, read_only: bool, input: Keys) -> Arc<Hub> {
        let activity = Activity::default();
        activity.reset(width, height);
        Arc::new(Hub {
            clients: Mutex::new(Vec::new()),
            next_id: AtomicU64::new(1),
            stats: Stats::default(),
            width,
            height,
            stripes,
            read_only,
            epoch: Instant::now(),
            waker: Wake::default(),
            stop: AtomicBool::new(false),
            input: Mutex::new(input),
            snapshots: Mutex::new(Vec::new()),
            waiters: AtomicUsize::new(0),
            activity,
        })
    }

    pub fn wake(&self) {
        self.waker.signal();
    }

    pub fn stopping(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    pub fn add_client(&self, tx: Sender<Out>, stream: TcpStream) -> Arc<Client> {
        let c = Arc::new(Client {
            id: self.next_id.fetch_add(1, Ordering::Relaxed),
            tx: Mutex::new(tx),
            stream,
            video: AtomicBool::new(false),
            started: AtomicBool::new(false),
            needs_keyframe: AtomicBool::new(true),
            inflight: Mutex::new(VecDeque::new()),
            min_rtt_ms: AtomicU32::new(0),
        });
        lock(&self.clients).push(c.clone());
        c
    }

    pub fn remove_client(&self, id: u64) {
        lock(&self.clients).retain(|c| c.id != id);
        self.wake();
    }

    pub fn video_clients(&self) -> Vec<Arc<Client>> {
        lock(&self.clients).iter().filter(|c| c.video.load(Ordering::Relaxed)).cloned().collect()
    }

    /// The `hello`: geometry, stripes and what this host offers, which is the picture and input only.
    pub fn hello(&self) -> String {
        let stripes: Vec<String> = self.stripes.iter().map(|(y, h)| format!("[{y},{h}]")).collect();
        format!(
            "{{\"t\":\"hello\",\"w\":{},\"h\":{},\"os\":\"juno\",\"audio\":false,\"mic\":false,\"stripes\":[{}],\
             \"files\":{{\"upload\":false,\"download\":false,\"dir\":\"\"}}{}}}",
            self.width,
            self.height,
            stripes.join(","),
            if self.read_only { ",\"readOnly\":true" } else { "" }
        )
    }

    /// `/stats`, with voxl's names.
    pub fn stats_json(&self) -> String {
        let st = &self.stats;
        let clients: Vec<String> = lock(&self.clients)
            .iter()
            .map(|c| {
                let (n, age) = c.backlog();
                format!(
                    "{{\"id\":{},\"video\":{},\"inflight\":{n},\"oldest_ms\":{},\"min_rtt_ms\":{}}}",
                    c.id,
                    c.video.load(Ordering::Relaxed),
                    age.as_millis(),
                    c.min_rtt_ms.load(Ordering::Relaxed)
                )
            })
            .collect();
        let n = |a: &AtomicU64| a.load(Ordering::Relaxed);
        format!(
            "{{\"captured\":{},\"encoded\":{},\"bytes\":{},\"encode_us_total\":{},\"keyframes\":{},\"paintovers\":{},\
             \"flow_blocked\":{},\"clients\":[{}]}}",
            n(&st.captured),
            n(&st.encoded),
            n(&st.bytes),
            n(&st.encode_us),
            n(&st.keyframes),
            n(&st.paintovers),
            n(&st.flow_blocked),
            clients.join(",")
        )
    }
}
