//! The logger: level check, rate limit, record, socket, stderr fallback.

use std::fmt;
use std::io::Write;
use std::os::unix::net::UnixDatagram;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};

use crate::control::{CONTROL_PATH, Control, env_level};
use crate::encode::{self, Record};
use crate::level::Level;
use crate::rules::{self, Window};
use crate::value::Value;

/// The log daemon's datagram socket.
pub const SOCKET_PATH: &str = "/run/lucina/log.sock";
/// The flight recorder's boot id file (32 hex).
pub const BOOT_PATH: &str = "/run/lucina/flight.boot";
/// Records one source may send per window at `info` and below.
pub const BURST: u64 = 200;
/// Records one source may send per window at `warn`.
pub const BURST_WARN: u64 = 50;
/// Records one source may send per window at `error`.
pub const BURST_ERROR: u64 = 20;
/// The rate-limit window.
pub const WINDOW_MS: i64 = 10_000;

/// Wall-clock and monotonic (since boot) milliseconds.
pub type Clock = Box<dyn Fn() -> (u64, u64) + Send + Sync>;

/// Where the records of a test rig go instead of the socket, in order. See [`Config::sink`].
// Test builds only; production send() is unchanged.
#[cfg(test)]
pub type Sink = std::sync::Arc<std::sync::Mutex<Vec<String>>>;

/// Where a logger puts things. The defaults are Juno's; tests change them.
pub struct Config {
    pub source: String,
    pub socket_path: PathBuf,
    pub control_path: PathBuf,
    pub boot_path: PathBuf,
    /// The level when the control file says nothing; the default is `LUCINA_LOG`, else `info`.
    pub default_level: Option<Level>,
    pub clock: Clock,
    /// Where the line goes when the socket does not exist; the default is standard error.
    pub fallback: Box<dyn Write + Send>,
    /// Test seam: with this set every line goes to the `Vec` instead of the socket, and no socket is opened at all. A real
    /// queue holds only `net.unix.max_dgram_qlen` datagrams (10 by kernel default, 512 when systemd raises it), so counting
    /// what arrived over a real socket measures the host; counting what lands here measures the logger.
    #[cfg(test)]
    pub sink: Option<Sink>,
}

impl Config {
    pub fn new(source: &str) -> Config {
        Config {
            source: clean_source(source),
            socket_path: PathBuf::from(SOCKET_PATH),
            control_path: PathBuf::from(CONTROL_PATH),
            boot_path: PathBuf::from(BOOT_PATH),
            default_level: env_level(),
            clock: Box::new(real_clock),
            fallback: Box::new(std::io::stderr()),
            #[cfg(test)]
            sink: None,
        }
    }
}

fn clean_source(s: &str) -> String {
    let mut out: String = s
        .chars()
        .take(encode::MAX_KEY)
        .map(|c| match c.to_ascii_lowercase() {
            c @ ('a'..='z' | '0'..='9' | '_' | '-') => c,
            _ => '_',
        })
        .collect();
    if out.is_empty() {
        out.push_str("unknown");
    }
    out
}

/// The boot id from `path`: 32 hex digits, else `unknown`.
pub fn read_boot_id(path: &std::path::Path) -> String {
    match std::fs::read_to_string(path) {
        Ok(t) if t.trim().len() == 32 && t.trim().bytes().all(|b| b.is_ascii_hexdigit()) => {
            t.trim().to_ascii_lowercase()
        }
        _ => "unknown".to_owned(),
    }
}

/// Wall milliseconds since the epoch, and monotonic milliseconds since boot (suspend time counts on Linux).
pub fn real_clock() -> (u64, u64) {
    let wall = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    #[cfg(target_os = "linux")]
    let id = libc::CLOCK_BOOTTIME;
    #[cfg(not(target_os = "linux"))]
    let id = libc::CLOCK_MONOTONIC;
    // SAFETY: `ts` is a valid, writable timespec for the whole call; zero is a valid timespec to start from.
    let mono = unsafe {
        let mut ts: libc::timespec = std::mem::zeroed();
        if libc::clock_gettime(id, &mut ts) == 0 {
            (ts.tv_sec as u64).saturating_mul(1000) + (ts.tv_nsec as u64) / 1_000_000
        } else {
            0
        }
    };
    (wall, mono)
}

/// The rate-limit budgets are kept per level class: `info` and below, `warn`, `error` (docs/design/juno-logging.md section 4).
const BUCKETS: usize = 3;

fn bucket(level: Level) -> usize {
    match level {
        Level::Error => 2,
        Level::Warn => 1,
        _ => 0,
    }
}

fn burst_of(bucket: usize) -> u64 {
    [BURST, BURST_WARN, BURST_ERROR][bucket]
}

/// One level class's limiter state, and the `seq` of the first and last record it held back in the current window
/// (0 when none: `seq` starts at 1).
#[derive(Clone, Copy, Default)]
struct Bucket {
    window: Option<Window>,
    first_held: u64,
    last_held: u64,
}

struct State {
    control: Control,
    buckets: [Bucket; BUCKETS],
    sock: Option<UnixDatagram>,
    fallback: Box<dyn Write + Send>,
}

/// One source's logger.
pub struct Logger {
    source: String,
    boot: String,
    pid: u32,
    socket_path: PathBuf,
    clock: Clock,
    seq: AtomicU64,
    dropped: AtomicU64,
    fallbacks: AtomicU64,
    rate_limited: AtomicU64,
    state: Mutex<State>,
    /// Where the records go instead of the socket, when the test rig set [`Config::sink`].
    #[cfg(test)]
    sink: Option<Sink>,
}

static GLOBAL: OnceLock<Logger> = OnceLock::new();

impl Logger {
    /// Starts the process-wide logger for `source` (the first call wins; later ones return the same logger).
    pub fn init(source: &str) -> &'static Logger {
        GLOBAL.get_or_init(|| Logger::new(Config::new(source)))
    }

    /// A logger of its own (tests, and programs that log for several sources).
    pub fn new(cfg: Config) -> Logger {
        Logger {
            boot: read_boot_id(&cfg.boot_path),
            pid: std::process::id(),
            socket_path: cfg.socket_path,
            clock: cfg.clock,
            seq: AtomicU64::new(1),
            dropped: AtomicU64::new(0),
            fallbacks: AtomicU64::new(0),
            rate_limited: AtomicU64::new(0),
            state: Mutex::new(State {
                control: Control::new(cfg.control_path, cfg.default_level),
                buckets: [Bucket::default(); BUCKETS],
                sock: None,
                fallback: cfg.fallback,
            }),
            source: cfg.source,
            #[cfg(test)]
            sink: cfg.sink,
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The source name.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Records the socket could not take (missing, nobody listening, full).
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Records written to the fallback because the socket did not exist or had no listener.
    pub fn fallbacks(&self) -> u64 {
        self.fallbacks.load(Ordering::Relaxed)
    }

    /// Records the rate limit held back.
    pub fn rate_limited(&self) -> u64 {
        self.rate_limited.load(Ordering::Relaxed)
    }

    /// Whether a record at `level` would be logged now. This is the level check the macros make before formatting anything;
    /// it may re-read the control file (see [`crate::control`]).
    pub fn enabled(&self, level: Level) -> bool {
        let (wall, mono) = (self.clock)();
        let mut st = self.lock();
        st.control.refresh(to_ms(mono));
        // No timer: a window that ended with records held back is reported by the next log call of this logger.
        self.report_expired(&mut st, wall, mono, false);
        level <= st.control.effective(&self.source)
    }

    /// Logs `args` at `level` with `fields`, if the level is on. Same as what the macros do.
    pub fn log(&self, level: Level, args: fmt::Arguments<'_>, fields: &[(&str, Value)]) {
        if self.enabled(level) {
            self.emit(level, args, fields);
        }
    }

    /// Logs without the level check: the rate limit, the record and the send.
    ///
    /// Every call takes one `seq`, sent or held back, so a gap in `seq` is exactly the records the limiter held. The held
    /// count is reported by a `warn` record (`suppressed`, `from_seq`, `to_seq`, `interval_ms`, `cause`) before the next record
    /// that goes out, or by [`Logger::flush`].
    pub fn emit(&self, level: Level, args: fmt::Arguments<'_>, fields: &[(&str, Value)]) {
        // Format before taking the lock: a Display impl that logs must not deadlock.
        let msg = fmt::format(args);
        let (wall, mono) = (self.clock)();
        let mut st = self.lock();
        self.report_expired(&mut st, wall, mono, false);
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        let b = bucket(level);
        let verdict = rules::admit(st.buckets[b].window, to_ms(mono), WINDOW_MS, burst_of(b));
        st.buckets[b].window = Some(verdict.win);
        if verdict.report > 0 {
            let range = take_held(&mut st.buckets[b]);
            self.write_report(&mut st, wall, mono, verdict.report, range, WINDOW_MS);
        }
        if !verdict.admit {
            let bk = &mut st.buckets[b];
            if bk.first_held == 0 {
                bk.first_held = seq;
            }
            bk.last_held = seq;
            self.rate_limited.fetch_add(1, Ordering::Relaxed);
            return;
        }
        self.write(&mut st, wall, mono, seq, level, &msg, fields);
    }

    /// Reports what the limiter holds back right now, ended window or not. Call it before a program exits: nothing else
    /// reports a count when the source goes quiet (there is no timer). Dropping a logger does it too.
    pub fn flush(&self) {
        let (wall, mono) = (self.clock)();
        let mut st = self.lock();
        self.report_expired(&mut st, wall, mono, true);
    }

    /// Writes the report of every level class whose window has ended (or all with records held, when `force`).
    fn report_expired(&self, st: &mut State, wall: u64, mono: u64, force: bool) {
        let now = to_ms(mono);
        for b in 0..BUCKETS {
            let Some(w) = st.buckets[b].window else {
                continue;
            };
            if w.suppressed == 0 {
                continue;
            }
            let ended = w.start_ms > now || now - w.start_ms >= WINDOW_MS;
            if !ended && !force {
                continue;
            }
            let interval = if ended { WINDOW_MS } else { now - w.start_ms };
            let range = take_held(&mut st.buckets[b]);
            // The count now rides on the report; the window keeps its start and its admitted count.
            st.buckets[b].window = Some(Window { suppressed: 0, ..w });
            self.write_report(st, wall, mono, w.suppressed, range, interval);
        }
    }

    /// The suppression record: `warn`, written without being counted against any budget.
    fn write_report(
        &self,
        st: &mut State,
        wall: u64,
        mono: u64,
        count: u64,
        (from, to): (u64, u64),
        interval_ms: i64,
    ) {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        let text = format!("{count} lines suppressed");
        let f = [
            ("suppressed", Value::UInt(count)),
            ("from_seq", Value::UInt(from)),
            ("to_seq", Value::UInt(to)),
            ("interval_ms", Value::Int(interval_ms)),
            ("cause", Value::from("rate")),
        ];
        self.write(st, wall, mono, seq, Level::Warn, &text, &f);
    }

    #[allow(clippy::too_many_arguments)]
    fn write(
        &self,
        st: &mut State,
        wall: u64,
        mono: u64,
        seq: u64,
        level: Level,
        msg: &str,
        fields: &[(&str, Value)],
    ) {
        let line = encode::encode(&Record {
            wall_ms: wall,
            mono_ms: mono,
            boot: &self.boot,
            seq,
            level,
            source: &self.source,
            pid: self.pid,
            msg,
            fields,
        });
        self.send(st, &line);
    }

    /// One non-blocking datagram. Never blocks, never panics.
    fn send(&self, st: &mut State, line: &str) {
        // Test builds only; production send() is unchanged.
        #[cfg(test)]
        if let Some(sink) = &self.sink {
            // The seam: nothing is dropped and nothing reaches the fallback; the line is exactly what the socket would have got.
            sink.lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(line.to_owned());
            return;
        }
        if st.sock.is_none()
            && let Ok(s) = UnixDatagram::unbound()
            && s.set_nonblocking(true).is_ok()
        {
            st.sock = Some(s);
        }
        let result = match &st.sock {
            Some(s) => s.send_to(line.as_bytes(), &self.socket_path),
            None => Err(std::io::ErrorKind::NotFound.into()),
        };
        let Err(e) = result else { return };
        self.dropped.fetch_add(1, Ordering::Relaxed);
        use std::io::ErrorKind::{ConnectionRefused, NotFound};
        if matches!(e.kind(), NotFound | ConnectionRefused) {
            self.fallbacks.fetch_add(1, Ordering::Relaxed);
            let _ = st.fallback.write_all(line.as_bytes());
            let _ = st.fallback.write_all(b"\n");
        }
    }
}

/// The `seq` range of the records held back since the last report; empties it.
fn take_held(b: &mut Bucket) -> (u64, u64) {
    let range = (b.first_held, b.last_held);
    b.first_held = 0;
    b.last_held = 0;
    range
}

impl Drop for Logger {
    fn drop(&mut self) {
        self.flush();
    }
}

fn to_ms(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

/// The process-wide logger; started as `unknown` if [`Logger::init`] was never called.
pub fn global() -> &'static Logger {
    GLOBAL.get_or_init(|| Logger::new(Config::new("unknown")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::testdir::TempDir;
    use crate::value::redacted;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;

    #[derive(Clone, Default)]
    struct Buf(Arc<Mutex<Vec<u8>>>);
    impl Write for Buf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl Buf {
        fn text(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }
    }

    /// A logger over a temp dir with a clock the test moves.
    struct Rig {
        dir: TempDir,
        mono: Arc<AtomicU64>,
        err: Buf,
        sink: Option<Sink>,
        log: Logger,
    }

    impl Rig {
        /// Every line written since the last call, and forget them: the in-memory answer to `drain`.
        fn lines(&self) -> Vec<String> {
            let Some(sink) = &self.sink else {
                panic!("this rig writes to the socket; drain the receiver instead");
            };
            std::mem::take(&mut *sink.lock().unwrap_or_else(|e| e.into_inner()))
        }
    }

    /// A rig over a real socket: how many records arrive is the host's queue, so use it only where that is the point.
    fn rig(source: &str) -> Rig {
        rig_over(source, None)
    }

    /// A rig whose records go to an in-memory sink instead of the socket: only the rate limiter decides what is written, so the
    /// counts do not depend on the host's `net.unix.max_dgram_qlen` (kernel default 10, buildhost 512).
    fn sink_rig(source: &str) -> Rig {
        rig_over(source, Some(Arc::new(Mutex::new(Vec::new()))))
    }

    fn rig_over(source: &str, sink: Option<Sink>) -> Rig {
        let dir = TempDir::new("logger");
        let mono = Arc::new(AtomicU64::new(100_000));
        let m = mono.clone();
        let err = Buf::default();
        let mut cfg = Config::new(source);
        cfg.socket_path = dir.path().join("log.sock");
        cfg.control_path = dir.path().join("levels");
        cfg.boot_path = dir.path().join("flight.boot");
        cfg.default_level = None;
        cfg.clock = Box::new(move || (1_700_000_000_000, m.load(Ordering::Relaxed)));
        cfg.fallback = Box::new(err.clone());
        cfg.sink = sink.clone();
        let log = Logger::new(cfg);
        Rig {
            dir,
            mono,
            err,
            sink,
            log,
        }
    }

    fn receiver(r: &Rig) -> UnixDatagram {
        let s = UnixDatagram::bind(r.dir.path().join("log.sock")).unwrap();
        s.set_nonblocking(true).unwrap();
        s
    }

    fn drain(s: &UnixDatagram) -> Vec<String> {
        let mut out = Vec::new();
        let mut buf = [0u8; 4096];
        while let Ok(n) = s.recv(&mut buf) {
            out.push(String::from_utf8(buf[..n].to_vec()).unwrap());
        }
        out
    }

    /// The end-to-end socket test: 8 records, under the 11 that the kernel default queue (10, plus the one in flight) takes, so
    /// every one arrives on any host that leaves `net.unix.max_dgram_qlen` at or above its default; measured floor is 7. No test
    /// counts more arrived records than that; a full queue is the two socket tests below, which claim no count.
    #[test]
    fn socket_round_trip() {
        let r = rig("Shell");
        let rx = receiver(&r);
        r.log.log(
            Level::Info,
            format_args!("hello {}", 7),
            &[("a", Value::from(1u8))],
        );
        r.log.log(Level::Warn, format_args!("second"), &[]);
        let got = drain(&rx);
        assert_eq!(got.len(), 2);
        assert!(got[0].starts_with("{\"t\":1700000000000,\"m\":100000,\"boot\":\"unknown\",\"seq\":1,\"lvl\":\"info\",\"src\":\"shell\",\"pid\":"));
        assert!(got[0].contains("\"msg\":\"hello 7\",\"f\":{\"a\":1}}"));
        assert!(got[1].contains("\"seq\":2,\"lvl\":\"warn\""));
        assert_eq!(r.log.dropped(), 0);
        assert!(r.err.text().is_empty());
        // the small burst: all of it in order, nothing dropped, nothing to the fallback
        for i in 3..=8 {
            r.log.log(Level::Info, format_args!("line {i}"), &[]);
        }
        let more = drain(&rx);
        assert_eq!(more.len(), 6, "the burst of 8 must fit the host's queue");
        for (n, line) in more.iter().enumerate() {
            let seq = 3 + n;
            assert!(
                line.contains(&format!("\"seq\":{seq},"))
                    && line.contains(&format!("\"msg\":\"line {seq}\"")),
                "{line}"
            );
        }
        assert_eq!(r.log.dropped(), 0);
        assert_eq!(r.log.fallbacks(), 0);
        assert!(r.err.text().is_empty());
    }

    #[test]
    fn boot_id_is_read_when_valid() {
        let dir = TempDir::new("boot");
        let p = dir.path().join("flight.boot");
        assert_eq!(read_boot_id(&p), "unknown");
        std::fs::write(&p, "0123456789ABCDEF0123456789abcdef\n").unwrap();
        assert_eq!(read_boot_id(&p), "0123456789abcdef0123456789abcdef");
        std::fs::write(&p, "not hex not hex not hex not hex!").unwrap();
        assert_eq!(read_boot_id(&p), "unknown");
        std::fs::write(&p, "abc").unwrap();
        assert_eq!(read_boot_id(&p), "unknown");
    }

    #[test]
    fn missing_socket_falls_back_to_stderr_and_counts() {
        let r = rig("shell");
        r.log.log(Level::Info, format_args!("nobody home"), &[]);
        assert_eq!(r.log.dropped(), 1);
        assert_eq!(r.log.fallbacks(), 1);
        let t = r.err.text();
        assert!(
            t.ends_with("\n") && t.contains("\"msg\":\"nobody home\""),
            "{t}"
        );
    }

    #[test]
    fn dead_socket_file_falls_back_too() {
        let r = rig("shell");
        drop(receiver(&r)); // the file stays, nobody listens
        r.log.log(Level::Info, format_args!("x"), &[]);
        assert_eq!(r.log.dropped(), 1);
        assert_eq!(r.log.fallbacks(), 1);
    }

    #[test]
    fn full_socket_drops_without_blocking_or_stderr() {
        let r = rig("shell");
        let rx = receiver(&r);
        let big = "x".repeat(1500);
        // the queue is small; a few thousand records always fill it, and the rate limit would stop us first, so move the window
        for i in 0..5000u64 {
            r.mono.store(100_000 + i * 20_000, Ordering::Relaxed);
            r.log.log(Level::Info, format_args!("{big}"), &[]);
        }
        assert!(r.log.dropped() > 0, "a full queue must have dropped some");
        assert_eq!(r.log.fallbacks(), 0, "a full socket is not a missing one");
        assert!(r.err.text().is_empty());
        assert!(!drain(&rx).is_empty());
    }

    /// The drop path pinned on its own. A receiver that is never read takes what its queue holds and no more, and how many that
    /// is belongs to the host (`net.unix.max_dgram_qlen`: 10 by kernel default, 512 on buildhost, and the socket receive buffer
    /// cuts below that for big records), so the claim is the accounting rather than a count.
    #[test]
    fn dropped_counts_every_record_the_queue_refused() {
        let r = rig("shell");
        let rx = receiver(&r);
        let sent = 600u64;
        let big = "x".repeat(1500);
        for i in 0..sent {
            // a window of its own each time: only the socket refuses, the rate limiter never does
            r.mono.store(100_000 + i * 20_000, Ordering::Relaxed);
            r.log.log(Level::Info, format_args!("{big}"), &[]);
        }
        let got = drain(&rx);
        assert_eq!(
            got.len() as u64 + r.log.dropped(),
            sent,
            "queued {} + dropped {} is not what was sent",
            got.len(),
            r.log.dropped()
        );
        // 600 records of 1500 bytes are more than any queue: `max_dgram_qlen` stops the count well below this
        assert!(r.log.dropped() > 0);
        assert_eq!(r.log.fallbacks(), 0, "a full socket is not a missing one");
        assert!(r.err.text().is_empty());
    }

    #[test]
    fn levels_follow_the_control_file_lazily() {
        let r = rig("shell");
        let rx = receiver(&r);
        assert!(!r.log.enabled(Level::Debug));
        assert!(r.log.enabled(Level::Info));
        std::fs::write(r.dir.path().join("levels"), "# x\nshell=debug\n").unwrap();
        r.mono.fetch_add(500, Ordering::Relaxed);
        assert!(
            !r.log.enabled(Level::Debug),
            "half a second: not re-read yet"
        );
        r.mono.fetch_add(600, Ordering::Relaxed);
        assert!(r.log.enabled(Level::Debug));
        assert!(!r.log.enabled(Level::Trace));
        r.log.log(Level::Debug, format_args!("now on"), &[]);
        assert_eq!(drain(&rx).len(), 1);
        r.log.log(Level::Trace, format_args!("still off"), &[]);
        assert!(drain(&rx).is_empty());
    }

    #[test]
    fn disabled_levels_do_not_format_or_count() {
        struct Boom;
        impl fmt::Display for Boom {
            fn fmt(&self, _: &mut fmt::Formatter<'_>) -> fmt::Result {
                panic!("formatted while disabled")
            }
        }
        let r = rig("shell");
        r.log.log(Level::Trace, format_args!("{}", Boom), &[]);
        assert_eq!(r.log.dropped(), 0);
        assert_eq!(r.log.rate_limited(), 0);
    }

    // Counts the limiter's decisions through the test sink; the socket transport is covered by socket_round_trip,
    // dropped_counts_every_record_the_queue_refused, missing_socket_falls_back_to_stderr_and_counts,
    // dead_socket_file_falls_back_too and full_socket_drops_without_blocking_or_stderr.
    #[test]
    fn rate_limit_then_one_suppressed_record() {
        let r = sink_rig("shell");
        for i in 0..250 {
            r.log.log(Level::Info, format_args!("line {i}"), &[]);
        }
        let got = r.lines();
        assert_eq!(got.len(), 200, "burst of 200");
        assert_eq!(r.log.rate_limited(), 50);
        // nothing is emitted by a timer: the report waits for the next log call after the window
        r.mono.fetch_add(10_000, Ordering::Relaxed);
        assert!(r.lines().is_empty());
        r.log.log(Level::Info, format_args!("after"), &[]);
        let got = r.lines();
        assert_eq!(got.len(), 2);
        assert!(
            got[0].contains("\"msg\":\"50 lines suppressed\""),
            "{}",
            got[0]
        );
        assert!(got[0].contains("\"lvl\":\"warn\""));
        assert!(got[0].contains(
            "\"f\":{\"suppressed\":50,\"from_seq\":201,\"to_seq\":250,\"interval_ms\":10000,\"cause\":\"rate\"}"
        ));
        assert!(got[1].contains("\"msg\":\"after\""));
        // held records took seq 201..=250; the report and the next record follow
        assert!(got[0].contains("\"seq\":251,") && got[1].contains("\"seq\":252,"));
    }

    // Counts the limiter's decisions through the test sink; the socket transport is covered by socket_round_trip,
    // dropped_counts_every_record_the_queue_refused, missing_socket_falls_back_to_stderr_and_counts,
    // dead_socket_file_falls_back_too and full_socket_drops_without_blocking_or_stderr.
    #[test]
    fn budgets_are_per_level() {
        let r = sink_rig("shell");
        for i in 0..300 {
            r.log.log(Level::Info, format_args!("i{i}"), &[]);
        }
        // info is exhausted; warn and error have budgets of their own
        for i in 0..60 {
            r.log.log(Level::Warn, format_args!("w{i}"), &[]);
        }
        for i in 0..25 {
            r.log.log(Level::Error, format_args!("e{i}"), &[]);
        }
        let got = r.lines();
        let count = |l: &str| {
            got.iter()
                .filter(|x| x.contains(&format!("\"lvl\":\"{l}\"")))
                .count()
        };
        assert_eq!(count("info"), 200);
        assert_eq!(count("warn"), 50);
        assert_eq!(count("error"), 20);
        assert_eq!(r.log.rate_limited(), 100 + 10 + 5);
    }

    // Counts the limiter's decisions through the test sink; the socket transport is covered by socket_round_trip,
    // dropped_counts_every_record_the_queue_refused, missing_socket_falls_back_to_stderr_and_counts,
    // dead_socket_file_falls_back_too and full_socket_drops_without_blocking_or_stderr.
    #[test]
    fn a_quiet_source_is_reported_by_the_next_call_of_any_level_and_by_flush() {
        let r = sink_rig("shell");
        for i in 0..210 {
            r.log.log(Level::Info, format_args!("{i}"), &[]);
        }
        assert_eq!(r.lines().len(), 200);
        // window ended, no new info record: a call at another level (even a disabled one) carries the report
        r.mono.fetch_add(10_000, Ordering::Relaxed);
        assert!(!r.log.enabled(Level::Trace));
        let got = r.lines();
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].contains("\"msg\":\"10 lines suppressed\""));
        assert!(got[0].contains("\"from_seq\":201,\"to_seq\":210,\"interval_ms\":10000"));
        // reported once only
        r.log.log(Level::Warn, format_args!("w"), &[]);
        assert_eq!(r.lines().len(), 1);
        // flush reports a window that has not ended yet, once
        for i in 0..205 {
            r.log.log(Level::Error, format_args!("{i}"), &[]);
        }
        assert_eq!(r.lines().len(), 20, "the error burst took its own budget");
        r.mono.fetch_add(3_000, Ordering::Relaxed);
        r.log.flush();
        let got = r.lines();
        assert_eq!(got.len(), 1);
        assert!(
            got[0].contains("\"msg\":\"185 lines suppressed\"")
                && got[0].contains("\"interval_ms\":3000"),
            "{got:?}"
        );
        r.log.flush();
        assert!(r.lines().is_empty());
    }

    // Counts the limiter's decisions through the test sink; the socket transport is covered by socket_round_trip,
    // dropped_counts_every_record_the_queue_refused, missing_socket_falls_back_to_stderr_and_counts,
    // dead_socket_file_falls_back_too and full_socket_drops_without_blocking_or_stderr.
    #[test]
    fn no_report_when_nothing_was_suppressed() {
        let r = sink_rig("shell");
        for i in 0..200 {
            r.log.log(Level::Info, format_args!("{i}"), &[]);
        }
        r.mono.fetch_add(10_000, Ordering::Relaxed);
        r.log.log(Level::Info, format_args!("next"), &[]);
        let got = r.lines();
        assert_eq!(got.len(), 201);
        assert!(!got.iter().any(|l| l.contains("suppressed")));
    }

    #[test]
    fn secrets_never_reach_the_output() {
        let secret = String::from("hunter2-correct-horse");
        let r = rig("shell");
        let rx = receiver(&r);
        let s = redacted(&secret);
        let strs = redacted("hunter2-correct-horse");
        // every way the API offers to carry one
        r.log.log(
            Level::Info,
            format_args!("a {s} {s:?} {s:#?} {strs:>30}"),
            &[("pw", Value::from(s))],
        );
        r.log.log(
            Level::Info,
            format_args!("b"),
            &[
                ("pw", Value::from(redacted(&secret))),
                ("x", Value::Redacted),
            ],
        );
        r.log.log(
            Level::Info,
            format_args!("{:?}", Value::from(s)),
            &[("k", crate::value::Value::from(strs))],
        );
        let all = drain(&rx).join("\n");
        assert!(all.contains("<redacted>"));
        assert!(!all.contains("hunter2"), "{all}");
        assert!(!all.contains("correct-horse"));
        assert_eq!(std::mem::size_of_val(&s), 0, "a Redacted keeps nothing");
        assert_eq!(format!("{s}{s:?}"), "<redacted><redacted>");
    }

    #[test]
    fn source_names_are_cleaned() {
        assert_eq!(clean_source("Shell"), "shell");
        assert_eq!(clean_source("a b/c"), "a_b_c");
        assert_eq!(clean_source(""), "unknown");
        assert_eq!(clean_source(&"x".repeat(50)).len(), 32);
    }

    #[test]
    fn global_logger_is_one_instance() {
        let a = Logger::init("first");
        let b = Logger::init("second");
        assert!(std::ptr::eq(a, b));
        assert!(std::ptr::eq(a, global()));
    }
}
