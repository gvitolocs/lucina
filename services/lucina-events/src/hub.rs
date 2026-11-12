// The one queue: `lucina-eventd` keeps the events in a window, applies the rules from [`crate::rules`], and serves the two
// sockets. Programs connect to [`Config::socket`], root readers to [`Config::read_socket`], the kernel's device is read on a
// thread of its own.

use std::collections::{HashMap, VecDeque};
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::io::{AsRawFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

use serde_json::{Map, Value};

use std::sync::atomic::{AtomicU64, Ordering};

use crate::rules;
use crate::wire::{self, Batch, Event, Failure, Field, Filter, Settled, Snapshot, Source, Waited};

/// Which source, and which process (peer pid, or a connection counter when the peer cred fails).
pub type SourceKey = (Source, u64);

#[derive(Clone)]
pub struct Config {
    pub cap: u64,
    pub max_read: u64,
    pub every_ms: u64,
    pub burst: u32,
    pub socket: PathBuf,
    pub read_socket: PathBuf,
    pub kernel: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            cap: 1024,
            max_read: 64,
            every_ms: 100,
            burst: 20,
            socket: wire::SOCKET.into(),
            read_socket: wire::READ_SOCKET.into(),
            kernel: Some(wire::KERNEL_DEVICE.into()),
        }
    }
}

/// Why an event did not go in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    InvalidKind,
    Stale,
    Flooded,
}

struct State {
    window: rules::Window,
    events: VecDeque<Event>,
    last: HashMap<SourceKey, u64>,
    buckets: HashMap<String, rules::Bucket>,
    present_seq: u64,
    last_present_ms: u64,
    present_app: Option<String>,
    presents: VecDeque<(u64, [i32; 4])>,
    dropped: u64,
    stale: u64,
    gaps: u64,
    flooded: u64,
    refused: u64,
    flood_pending: HashMap<String, (u64, u64)>,
    /// The last accepted event of each sticky kind+subject, kept outside the queue so it survives drops.
    sticky: HashMap<(String, Option<String>), Event>,
}

pub struct Hub {
    cfg: Config,
    clock: Box<dyn Fn() -> u64 + Send + Sync>,
    state: Mutex<State>,
    cond: Condvar,
}

/// Milliseconds since boot.
pub fn monotonic_ms() -> u64 {
    let mut ts: libc::timespec = unsafe { std::mem::zeroed() };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    (ts.tv_sec as u64) * 1000 + (ts.tv_nsec as u64) / 1_000_000
}

fn cut_value(v: Value) -> Value {
    match v {
        Value::String(s) => {
            if s.len() <= wire::MAX_VALUE {
                Value::String(s)
            } else {
                let mut end = wire::MAX_VALUE;
                while !s.is_char_boundary(end) {
                    end -= 1;
                }
                let mut cut = s[..end].to_string();
                cut.push('…');
                Value::String(cut)
            }
        }
        other => other,
    }
}

impl Hub {
    pub fn new(cfg: Config, clock: impl Fn() -> u64 + Send + Sync + 'static) -> Hub {
        Hub {
            cfg,
            clock: Box::new(clock),
            state: Mutex::new(State {
                window: rules::Window { head: 0, next: 0 },
                events: VecDeque::new(),
                last: HashMap::new(),
                buckets: HashMap::new(),
                present_seq: 0,
                last_present_ms: 0,
                present_app: None,
                presents: VecDeque::new(),
                dropped: 0,
                stale: 0,
                gaps: 0,
                flooded: 0,
                refused: 0,
                flood_pending: HashMap::new(),
                sticky: HashMap::new(),
            }),
            cond: Condvar::new(),
        }
    }

    fn now(&self) -> u64 {
        (self.clock)()
    }

    fn push_event(&self, st: &mut State, ev: Event) {
        let p = rules::push(st.window, self.cfg.cap);
        if p.dropped {
            st.dropped += 1;
            st.events.pop_front();
        }
        st.window = p.window;
        st.events.push_back(ev);
        debug_assert_eq!(st.events.len() as u64, st.window.next - st.window.head);
        debug_assert_eq!(st.events.front().unwrap().seq, st.window.head);
    }

    fn lost_event(
        seq: u64,
        src: Source,
        app: Option<&str>,
        src_seq: u64,
        t_ms: u64,
        count: u64,
        reason: &str,
    ) -> Event {
        let mut fields = Map::new();
        fields.insert("count".to_string(), Value::from(count));
        fields.insert("reason".to_string(), Value::from(reason));
        Event {
            seq,
            src,
            src_seq,
            t_ms,
            kind: "events.lost".to_string(),
            app: app.map(|a| a.to_string()),
            subject: None,
            fields,
        }
    }

    /// Take one event from a source, in this order: kind, recordability, per-source order, flood guard, fields, push.
    #[allow(clippy::too_many_arguments)]
    pub fn accept(
        &self,
        src: Source,
        app: Option<&str>,
        introspect: u8,
        src_key: SourceKey,
        src_seq: u64,
        kind: &str,
        subject: Option<&str>,
        fields: Vec<Field>,
    ) -> Result<(), Refusal> {
        if !rules::valid_kind(kind.as_bytes()) {
            self.state.lock().unwrap().refused += 1;
            return Err(Refusal::InvalidKind);
        }
        if !rules::may_record(src.code(), introspect) {
            return Ok(());
        }
        // A shell or kernel event about an app says which app in its `app` field.
        let field_app = || {
            fields
                .iter()
                .find(|f| f.n == "app")
                .and_then(|f| f.v.as_str())
                .filter(|s| valid_app_id(s))
        };
        let app: Option<&str> = match app {
            Some(a) => Some(a),
            None if src == Source::Shell || src == Source::Kernel => field_app(),
            None => None,
        };
        let mut st = self.state.lock().unwrap();
        let now = self.now();
        match rules::order(st.last.get(&src_key).copied(), src_seq) {
            rules::Order::Stale => {
                st.stale += 1;
                return Err(Refusal::Stale);
            }
            rules::Order::Gap(n) => {
                st.gaps += n;
                let ev = Self::lost_event(st.window.next, src, app, src_seq - n, now, n, "gap");
                self.push_event(&mut st, ev);
            }
            rules::Order::InOrder => {}
        }
        if (src == Source::App || src == Source::Toolkit)
            && let Some(app) = app
        {
            let bucket = st.buckets.get(app).copied().unwrap_or(rules::Bucket {
                tokens: self.cfg.burst,
                at_ms: now,
            });
            let adm = rules::admit(bucket, now, self.cfg.every_ms, self.cfg.burst);
            if !adm.ok {
                st.flooded += 1;
                let entry = st.flood_pending.entry(app.to_string()).or_insert((now, 0));
                entry.1 += 1;
                // A flooded event still counts as seen: the next one of this source is in order, not a gap.
                st.last.insert(src_key, src_seq);
                return Err(Refusal::Flooded);
            }
            st.buckets.insert(app.to_string(), adm.bucket);
            // At most one flood note per second per app, carrying what was flooded since the last one.
            let entry = st.flood_pending.entry(app.to_string()).or_insert((now, 0));
            if entry.1 > 0 && now - entry.0 >= 1000 {
                let count = entry.1;
                entry.0 = now;
                entry.1 = 0;
                let ev =
                    Self::lost_event(st.window.next, src, Some(app), src_seq, now, count, "flood");
                self.push_event(&mut st, ev);
            }
        }
        let vis: Vec<u8> = fields.iter().map(|f| f.vis).collect();
        let keep = rules::exposed_fields(&vis, rules::content_free(kind.as_bytes()));
        let mut map = Map::new();
        for &i in keep.iter().take(wire::MAX_FIELDS) {
            let f = &fields[i];
            map.insert(f.n.clone(), cut_value(f.v.clone()));
        }
        let ev = Event {
            seq: st.window.next,
            src,
            src_seq,
            t_ms: now,
            kind: kind.to_string(),
            app: app.map(|a| a.to_string()),
            subject: subject.map(|s| s.to_string()),
            fields: map,
        };
        Self::sticky_update(&mut st, &ev);
        self.push_event(&mut st, ev);
        st.last.insert(src_key, src_seq);
        drop(st);
        self.cond.notify_all();
        Ok(())
    }

    /// Keep the last accepted event of each sticky kind+subject, outside the queue.
    fn sticky_update(st: &mut State, e: &Event) {
        let slot = |k: &str, s: Option<String>| (k.to_string(), s);
        match e.kind.as_str() {
            "app.focused" => {
                st.sticky.insert(slot("app.focused", None), e.clone());
            }
            "power.battery" => {
                st.sticky.insert(slot("power.battery", None), e.clone());
            }
            "power.charger" => {
                st.sticky.insert(slot("power.charger", None), e.clone());
            }
            "net.up" | "net.down" => {
                st.sticky.insert(slot("net", e.subject.clone()), e.clone());
            }
            "media.started" => {
                if let Some(app) = &e.app {
                    st.sticky
                        .insert(slot("media", Some(app.clone())), e.clone());
                }
            }
            "media.ended" | "media.stopped" => {
                if let Some(app) = &e.app {
                    st.sticky.remove(&slot("media", Some(app.clone())));
                }
            }
            "storage.mounted" => {
                st.sticky
                    .insert(slot("storage", e.subject.clone()), e.clone());
            }
            "storage.unmounted" => {
                st.sticky.remove(&slot("storage", e.subject.clone()));
            }
            _ => {}
        }
    }

    pub fn read(&self, since: u64, filter: &Filter, max: Option<u64>) -> Batch {
        let st = self.state.lock().unwrap();
        let plan = rules::read_plan(
            st.window,
            since,
            max.unwrap_or(self.cfg.max_read).min(self.cfg.max_read),
        );
        let events: Vec<Event> = st
            .events
            .iter()
            .skip((plan.start - st.window.head) as usize)
            .take(plan.count as usize)
            .filter(|e| filter.matches(e))
            .cloned()
            .collect();
        Batch {
            ok: true,
            events,
            next: plan.start + plan.count,
            lost: plan.lost,
            reset: plan.reset,
        }
    }

    pub fn wait(&self, since: u64, filter: &Filter, timeout_ms: u64) -> Waited {
        let deadline = self.now() + timeout_ms;
        let mut st = self.state.lock().unwrap();
        let mut cursor = since;
        let mut lost_total = 0u64;
        loop {
            let plan = rules::read_plan(st.window, cursor, self.cfg.max_read);
            lost_total += plan.lost;
            let found = st
                .events
                .iter()
                .skip((plan.start - st.window.head) as usize)
                .take(plan.count as usize)
                .find(|e| filter.matches(e))
                .cloned();
            if let Some(e) = found {
                return Waited {
                    ok: true,
                    event: Some(e.clone()),
                    timeout: false,
                    next: e.seq + 1,
                    lost: lost_total,
                };
            }
            cursor = plan.start + plan.count;
            let now = self.now();
            if now >= deadline {
                return Waited {
                    ok: true,
                    event: None,
                    timeout: true,
                    next: cursor,
                    lost: lost_total,
                };
            }
            let (g, _) = self
                .cond
                .wait_timeout(st, Duration::from_millis(deadline - now))
                .unwrap();
            st = g;
        }
    }

    pub fn present(&self, app: Option<&str>, _frame: u64, rects: &[[i32; 4]], now: u64) {
        let mut st = self.state.lock().unwrap();
        st.present_seq += 1;
        st.last_present_ms = now;
        st.present_app = app.map(|a| a.to_string());
        let u = rects
            .iter()
            .copied()
            .reduce(wire::union)
            .unwrap_or([0, 0, 0, 0]);
        let seq = st.present_seq;
        st.presents.push_back((seq, u));
        if st.presents.len() > 64 {
            st.presents.pop_front();
        }
        drop(st);
        self.cond.notify_all();
    }

    pub fn settle(&self, since: u64, settle_ms: u64, timeout_ms: u64) -> Settled {
        let deadline = self.now() + timeout_ms;
        let mut st = self.state.lock().unwrap();
        loop {
            let now = self.now();
            if rules::settled(since, st.present_seq, st.last_present_ms, now, settle_ms) {
                let mut rect: Option<[i32; 4]> = None;
                for (seq, r) in &st.presents {
                    if *seq > since {
                        rect = Some(match rect {
                            Some(u) => wire::union(u, *r),
                            None => *r,
                        });
                    }
                }
                return Settled {
                    ok: true,
                    settled: true,
                    present_seq: st.present_seq,
                    app: st.present_app.clone(),
                    rect,
                };
            }
            if now >= deadline {
                return Settled {
                    ok: true,
                    settled: false,
                    present_seq: st.present_seq,
                    app: st.present_app.clone(),
                    rect: None,
                };
            }
            // Wake when the screen may have settled, not only at the deadline: while no present arrives nobody
            // notifies the condvar.
            let wait_ms = if st.present_seq > since {
                let settle_at = st.last_present_ms + settle_ms;
                deadline.min(settle_at).saturating_sub(now).max(1)
            } else {
                deadline - now
            };
            let (g, _) = self
                .cond
                .wait_timeout(st, Duration::from_millis(wait_ms))
                .unwrap();
            st = g;
        }
    }

    /// The sticky events, sorted by seq.
    pub fn state(&self) -> Snapshot {
        let st = self.state.lock().unwrap();
        let mut events: Vec<Event> = st.sticky.values().cloned().collect();
        events.sort_by_key(|e| e.seq);
        Snapshot { ok: true, events }
    }

    pub fn stats(&self) -> Value {
        let st = self.state.lock().unwrap();
        serde_json::json!({
            "ok": true,
            "head": st.window.head,
            "next": st.window.next,
            "events": st.events.len(),
            "dropped": st.dropped,
            "stale": st.stale,
            "gaps": st.gaps,
            "flooded": st.flooded,
            "refused": st.refused,
            "present_seq": st.present_seq,
        })
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn peer_cred(fd: RawFd) -> Option<(u32, u32)> {
    // SAFETY: `ucred` is a struct of plain integers (pid, uid, gid), so all-bits-zero is a valid
    // value; `getsockopt` overwrites it on success.
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let r = unsafe {
        // SAFETY: `fd` is a descriptor the caller owns and keeps open across the call, `cred` is a
        // writable `ucred`, and `len` is its exact size: the two pointers are valid and aligned for
        // the whole call, which is what SO_PEERCRED fills in.
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut cred as *mut _ as *mut libc::c_void,
            &mut len,
        )
    };
    if r == 0 {
        Some((cred.uid, cred.pid as u32))
    } else {
        None
    }
}

#[cfg(target_os = "macos")]
fn peer_cred(fd: RawFd) -> Option<(u32, u32)> {
    // The macOS pair of SO_PEERCRED: getpeereid for the credentials (SOCK_STREAM only, as here),
    // LOCAL_PEERPID for the pid, which getpeereid does not give.
    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    // SAFETY: `uid` and `gid` are writable locals of exactly the `uid_t`/`gid_t` the out-params
    // take, and `fd` is a descriptor the caller owns and keeps open across the call. getpeereid
    // writes both only on success (0); on failure it returns -1.
    if unsafe { libc::getpeereid(fd, &mut uid, &mut gid) } != 0 {
        return None;
    }
    let mut pid: libc::pid_t = 0;
    let mut len = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
    // SAFETY: as above, and LOCAL_PEERPID wants exactly one `pid_t`: the buffer is writable for
    // the `len` bytes given, so the kernel never writes past it.
    let r = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_LOCAL,
            libc::LOCAL_PEERPID,
            &mut pid as *mut libc::pid_t as *mut libc::c_void,
            &mut len,
        )
    };
    if r == 0 {
        Some((uid, pid as u32))
    } else {
        None
    }
}

/// Peer credentials are unknown here, so `None`: both callers treat it as untrusted and refuse
/// what needs a credential (handle_read_conn drops the connection unless the uid is 0, and a
/// Source::Shell hello is refused too); a plain program connection keeps serving, keyed by a
/// per-connection counter instead of a pid, which costs it only per-process flood accounting.
#[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
fn peer_cred(_fd: RawFd) -> Option<(u32, u32)> {
    None
}

fn set_mode(path: &std::path::Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
}

/// Read one `\n`-terminated line from a buffered reader; `Ok(None)` on a clean EOF, an error past [`wire::MAX_LINE`].
fn read_line(reader: &mut BufReader<UnixStream>, buf: &mut Vec<u8>) -> io::Result<Option<()>> {
    buf.clear();
    loop {
        let n = reader.read_until(b'\n', buf)?;
        if n == 0 {
            return if buf.is_empty() {
                Ok(None)
            } else {
                Ok(Some(()))
            };
        }
        if buf.last() == Some(&b'\n') {
            buf.pop();
            return Ok(Some(()));
        }
        if buf.len() > wire::MAX_LINE {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "line too long"));
        }
    }
}

fn write_line(stream: &mut UnixStream, v: &Value) -> io::Result<()> {
    let mut line =
        serde_json::to_string(v).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    line.push('\n');
    stream.write_all(line.as_bytes())
}

fn valid_app_id(app: &str) -> bool {
    !app.is_empty()
        && app.len() <= 64
        && app
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

fn shell_comm_ok(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/comm"))
        .map(|c| c.trim() == "lucina-shell")
        .unwrap_or(false)
}

fn handle_program_conn(stream: UnixStream, hub: Arc<Hub>, _cfg: Config) {
    let cred = peer_cred(stream.as_raw_fd());
    let mut reader = BufReader::new(stream);
    let mut buf: Vec<u8> = Vec::new();
    let hello: wire::In = match read_line(&mut reader, &mut buf) {
        Ok(Some(())) => match serde_json::from_slice(&buf) {
            Ok(h @ wire::In::Hello { .. }) => h,
            _ => return,
        },
        _ => return,
    };
    let (src, app, introspect) = match hello {
        wire::In::Hello {
            src,
            app,
            introspect,
        } => (src, app, introspect),
        _ => return,
    };
    if src == Source::Kernel {
        return;
    }
    if src == Source::Shell {
        let ok = cred.is_some_and(|(uid, pid)| uid == 0 && shell_comm_ok(pid));
        if !ok {
            return;
        }
    }
    if src == Source::App || src == Source::Toolkit {
        match &app {
            Some(a) if valid_app_id(a) => {}
            _ => return,
        }
    }
    let introspect = match introspect.as_deref() {
        Some("tree") => rules::INTROSPECT_TREE,
        Some("none") => rules::INTROSPECT_NONE,
        _ => rules::INTROSPECT_ALL,
    };
    static CONN_COUNTER: AtomicU64 = AtomicU64::new(0);
    let pid = cred
        .map(|(_, p)| p as u64)
        .unwrap_or_else(|| 1_000_000 + CONN_COUNTER.fetch_add(1, Ordering::Relaxed));
    let key: SourceKey = (src, pid);
    loop {
        match read_line(&mut reader, &mut buf) {
            Ok(Some(())) => {}
            _ => return,
        }
        let line: Result<wire::In, _> = serde_json::from_slice(&buf);
        match line {
            Ok(wire::In::Emit {
                src_seq,
                kind,
                subject,
                fields,
            }) => {
                let _ = hub.accept(
                    src,
                    app.as_deref(),
                    introspect,
                    key,
                    src_seq,
                    &kind,
                    subject.as_deref(),
                    fields,
                );
            }
            Ok(wire::In::Present { frame, rects, .. }) => {
                hub.present(app.as_deref(), frame, &rects, monotonic_ms());
            }
            _ => return,
        }
    }
}

/// Whether a reader's filter names tree events (`node.*`, `view.*`): only then do apps pay for a tree after each paint.
pub fn wants_tree(filter: &Filter) -> bool {
    filter
        .kind
        .iter()
        .any(|k| k.starts_with("node.") || k.starts_with("view."))
}

/// Tells the apps, through the time of [`wire::TREE_WANTED`], that a reader asked for tree events just now.
fn mark_tree_wanted(filter: &Filter) {
    if wants_tree(filter)
        && let Ok(f) = File::options()
            .create(true)
            .append(true)
            .open(wire::TREE_WANTED)
    {
        let _ = f.set_modified(std::time::SystemTime::now());
    }
}

fn handle_read_conn(stream: UnixStream, hub: Arc<Hub>, _cfg: Config) {
    let cred = peer_cred(stream.as_raw_fd());
    if !cred.is_some_and(|(uid, _)| uid == 0) {
        return;
    }
    let mut reader = BufReader::new(stream);
    let mut buf: Vec<u8> = Vec::new();
    loop {
        match read_line(&mut reader, &mut buf) {
            Ok(Some(())) => {}
            _ => return,
        }
        let ask: wire::Ask = match serde_json::from_slice(&buf) {
            Ok(a) => a,
            Err(_) => {
                let _ = write_line(
                    reader.get_mut(),
                    &serde_json::to_value(Failure {
                        ok: false,
                        error: "bad line".to_string(),
                    })
                    .unwrap(),
                );
                continue;
            }
        };
        if let wire::Ask::Read { filter, .. } | wire::Ask::Wait { filter, .. } = &ask {
            mark_tree_wanted(filter);
        }
        match ask {
            wire::Ask::Read { since, filter, max } => {
                let answer = match filter.check() {
                    Ok(()) => serde_json::to_value(hub.read(since, &filter, max)).unwrap(),
                    Err(e) => serde_json::to_value(Failure {
                        ok: false,
                        error: e,
                    })
                    .unwrap(),
                };
                if write_line(reader.get_mut(), &answer).is_err() {
                    return;
                }
            }
            wire::Ask::Wait {
                since,
                filter,
                timeout_ms,
            } => {
                let answer = match filter.check() {
                    Ok(()) => serde_json::to_value(hub.wait(since, &filter, timeout_ms)).unwrap(),
                    Err(e) => serde_json::to_value(Failure {
                        ok: false,
                        error: e,
                    })
                    .unwrap(),
                };
                if write_line(reader.get_mut(), &answer).is_err() {
                    return;
                }
            }
            wire::Ask::Settle {
                since,
                settle_ms,
                timeout_ms,
            } => {
                let answer =
                    serde_json::to_value(hub.settle(since, settle_ms, timeout_ms)).unwrap();
                if write_line(reader.get_mut(), &answer).is_err() {
                    return;
                }
            }
            wire::Ask::Stats {} => {
                let answer = hub.stats();
                if write_line(reader.get_mut(), &answer).is_err() {
                    return;
                }
            }
            wire::Ask::State {} => {
                let answer = serde_json::to_value(hub.state()).unwrap();
                if write_line(reader.get_mut(), &answer).is_err() {
                    return;
                }
            }
        }
    }
}

fn accept_loop(listener: UnixListener, hub: Arc<Hub>, cfg: Config, is_read: bool) {
    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                let h = hub.clone();
                let c = cfg.clone();
                thread::spawn(move || {
                    if is_read {
                        handle_read_conn(s, h, c)
                    } else {
                        handle_program_conn(s, h, c)
                    }
                });
            }
            Err(_) => thread::sleep(Duration::from_millis(50)),
        }
    }
}

fn kernel_loop(hub: Arc<Hub>, path: PathBuf) {
    let key: SourceKey = (Source::Kernel, 0);
    loop {
        match File::open(&path) {
            Ok(file) => {
                for line in BufReader::new(file).lines() {
                    let line = match line {
                        Ok(l) => l,
                        Err(e) => {
                            eprintln!("lucina-eventd: read {}: {e}, retrying", path.display());
                            break;
                        }
                    };
                    if let Some(ke) = wire::parse_kernel(&line) {
                        let fields: Vec<Field> = ke
                            .fields
                            .into_iter()
                            .map(|(n, v)| Field {
                                n,
                                v,
                                vis: rules::VIS_PUBLIC,
                            })
                            .collect();
                        let _ = hub.accept(
                            Source::Kernel,
                            None,
                            rules::INTROSPECT_ALL,
                            key,
                            ke.src_seq,
                            &ke.kind,
                            ke.subject.as_deref(),
                            fields,
                        );
                    }
                }
                eprintln!("lucina-eventd: {} closed, retrying", path.display());
            }
            Err(e) => {
                eprintln!(
                    "lucina-eventd: cannot open {}: {e}, retrying",
                    path.display()
                );
            }
        }
        thread::sleep(Duration::from_secs(5));
    }
}

/// Bind both sockets and serve forever: a thread per connection, plus the kernel reader when the device exists.
pub fn serve(hub: Arc<Hub>, cfg: &Config) -> io::Result<()> {
    for path in [&cfg.socket, &cfg.read_socket] {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)?;
        }
        let _ = fs::remove_file(path);
    }
    let prog = UnixListener::bind(&cfg.socket)?;
    let rd = UnixListener::bind(&cfg.read_socket)?;
    set_mode(&cfg.socket, 0o666);
    set_mode(&cfg.read_socket, 0o600);
    if let Some(k) = &cfg.kernel {
        if k.exists() {
            let h = hub.clone();
            let k = k.clone();
            thread::spawn(move || kernel_loop(h, k));
        } else {
            eprintln!(
                "lucina-eventd: no kernel device at {}, skipping",
                k.display()
            );
        }
    }
    let h = hub.clone();
    let c = cfg.clone();
    thread::spawn(move || accept_loop(rd, h, c, true));
    accept_loop(prog, hub, cfg.clone(), false);
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::io::AsRawFd;

    use super::peer_cred;

    /// Both ends of a socketpair are this process, so the peer cred is our own uid and pid.
    #[test]
    fn peer_cred_of_a_socketpair_names_this_process() {
        let (a, _b) = std::os::unix::net::UnixStream::pair().unwrap();
        // SAFETY: both take no arguments and touch no memory; they read this process's own ids.
        // The uid is the effective one, which is what SO_PEERCRED and getpeereid both report.
        let mine = (unsafe { libc::geteuid() }, unsafe { libc::getpid() } as u32);
        assert_eq!(peer_cred(a.as_raw_fd()), Some(mine));
    }
}
