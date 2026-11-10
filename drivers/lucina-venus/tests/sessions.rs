//! A session opened and closed against a small firmware model of the block: what the machine gave the session, and what it got
//! back, when a session ends cleanly, and when the firmware refuses one of its commands. The model answers every session command
//! with a done message (with an error where a test asks for one), the flush with its answer, and the property query with the same
//! requirements table as `tests/oracle.rs`; a test may also have it send a sequence-changed event of a chosen size with an
//! SESSION_EMPTY_BUFFER answer.

use std::cell::RefCell;
use std::fmt::Write;
use std::rc::Rc;

use lucina_qcom_soc::io::RegIo;
use lucina_venus::{
    Hal, Result,
    cpu::Core,
    decode::{Event, Machine, Session},
    hfi::{self, msg},
    iface::{self, Iface, Shared},
    regs,
    rules::MAX_PACKET_WORDS,
    session::{self},
};

const DEVICE: u32 = 0xd000_0000;

struct State {
    mem: Vec<u32>,
    regs: Vec<u32>,
    pending: bool,
    out: String,
    /// The error the model puts in its SYS_SESSION_INIT answer.
    init_error: u32,
    /// The error the model puts in its SESSION_START answer.
    start_error: u32,
    /// The error the model puts in its SESSION_RELEASE_RESOURCES answer.
    release_error: u32,
    /// Whether the model answers SYS_SESSION_ABORT at all.
    abort_answered: bool,
    /// The picture size of each sequence-changed event the model sends, one per SESSION_EMPTY_BUFFER, in order.
    seq_events: Vec<(u32, u32)>,
    /// How many SESSION_EMPTY_BUFFER commands the model has answered.
    etbs: u32,
}

type S = Rc<RefCell<State>>;

struct Mem(S);

impl Shared for Mem {
    fn read32(&self, offset: u32) -> u32 {
        self.0.borrow().mem[offset as usize / 4]
    }
    fn write32(&self, offset: u32, value: u32) {
        self.0.borrow_mut().mem[offset as usize / 4] = value;
    }
    fn barrier(&self) {}
}

struct Regs(S);

fn qhdr(_st: &State, q: usize) -> usize {
    6 + 14 * q
}

fn put_message(st: &mut State, words: &[u32]) {
    let h = qhdr(st, 1);
    let ring = ((st.mem[h + 1] - DEVICE) / 4) as usize;
    let size = st.mem[h + 3] as usize;
    let wr = st.mem[h + 13] as usize;
    for (k, w) in words.iter().enumerate() {
        st.mem[ring + (wr + k) % size] = *w;
    }
    st.mem[h + 13] = ((wr + words.len()) % size) as u32;
    st.pending = true;
}

/// The requirements table of `tests/oracle.rs`: type, size, region size, count_min, count_min_host, count_actual, contiguous,
/// alignment.
const BUFREQ: [[u32; 8]; 8] = [
    [1, 1568768, 0, 2, 2, 4, 1, 4096],
    [2, 3248128, 0, 6, 6, 8, 1, 4096],
    [3, 3133440, 0, 6, 6, 8, 1, 4096],
    [6, 0x300000, 0, 1, 1, 1, 1, 4096],
    [7, 0x80000, 0, 1, 1, 1, 1, 4096],
    [8, 0, 0, 0, 0, 0, 1, 4096],
    [4, 0x100000, 0, 1, 1, 1, 1, 4096],
    [5, 0x40000, 0, 1, 1, 1, 1, 4096],
];

fn bufreq_message(sid: u32) -> Vec<u32> {
    let mut m = vec![20 + 32 * 8, hfi::msg::SESSION_PROPERTY_INFO, sid, 1, session::property::BUFFER_REQUIREMENTS];
    m.extend(BUFREQ.iter().flatten());
    m
}

/// The model's session firmware: every command answered with a 32-byte done message, the flush with its answer, the property
/// query with the requirements table. No sequence-changed event is ever sent.
fn session_firmware(st: &mut State, pkt: &[u32]) {
    use hfi::cmd;
    let sid = pkt[2];
    let done = |st: &mut State, kind: u32, error: u32| {
        let mut m = vec![0u32; 8];
        m[0] = 32;
        m[1] = kind;
        m[2] = sid;
        m[3] = error;
        put_message(st, &m);
    };
    match pkt[1] {
        cmd::SYS_SESSION_INIT => {
            let e = st.init_error;
            done(st, msg::SYS_SESSION_INIT, e)
        }
        cmd::SYS_SESSION_END => done(st, msg::SYS_SESSION_END, 0),
        cmd::SYS_SESSION_ABORT => {
            if st.abort_answered {
                done(st, msg::SYS_SESSION_ABORT, 0)
            }
        }
        cmd::SESSION_LOAD_RESOURCES => done(st, msg::SESSION_LOAD_RESOURCES, 0),
        cmd::SESSION_START => {
            let e = st.start_error;
            done(st, msg::SESSION_START, e)
        }
        cmd::SESSION_STOP => done(st, msg::SESSION_STOP, 0),
        cmd::SESSION_RELEASE_RESOURCES => {
            let e = st.release_error;
            done(st, msg::SESSION_RELEASE_RESOURCES, e)
        }
        cmd::SESSION_RELEASE_BUFFERS => done(st, msg::SESSION_RELEASE_BUFFERS, 0),
        cmd::SESSION_FLUSH => put_message(st, &[20, msg::SESSION_FLUSH, sid, 0, pkt[3]]),
        cmd::SESSION_GET_PROPERTY => put_message(st, &bufreq_message(sid)),
        cmd::SESSION_EMPTY_BUFFER => {
            put_message(st, &[36, msg::SESSION_EMPTY_BUFFER, sid, 0, pkt[8], pkt[10], pkt[11], pkt[12], 0]);
            if let Some(&(w, h)) = st.seq_events.get(st.etbs as usize) {
                // The event vector of `tests/oracle.rs`'s `session_firmware`, with the size replaced.
                let mut e = vec![0, msg::EVENT_NOTIFY, sid, session::event::SEQUENCE_CHANGED, 0x100_0001, 5,
                    0x1001, 3, w, h, 0x100_3007, 3, 0, 0x100_3009, 1, 0x120_300e, 5, 4, 4, 2, 6,
                    0x0700_000e, 28, 1, 1, 0, 0, w, h];
                e[0] = (e.len() * 4) as u32;
                put_message(st, &e);
            }
            st.etbs += 1;
        }
        _ => {}
    }
}

/// The model's firmware: the core's own commands, then the session's.
fn firmware(st: &mut State) {
    let h = qhdr(st, 0);
    let ring = ((st.mem[h + 1] - DEVICE) / 4) as usize;
    let size = st.mem[h + 3] as usize;
    while st.mem[h + 12] != st.mem[h + 13] {
        let rd = st.mem[h + 12] as usize;
        let n = (st.mem[ring + rd] / 4) as usize;
        let pkt: Vec<u32> = (0..n).map(|k| st.mem[ring + (rd + k) % size]).collect();
        let _ = writeln!(st.out, "cmd {}", pkt.iter().map(|w| format!("{:x}", w)).collect::<Vec<_>>().join(" "));
        st.mem[h + 12] = ((rd + n) % size) as u32;
        match pkt[1] {
            hfi::cmd::SYS_INIT => put_message(st, &[28, hfi::msg::SYS_INIT, 0, 1, 0x1009, 0x7012, 0x3002]),
            hfi::cmd::SYS_GET_PROPERTY => {
                let mut m = vec![16 + 128, hfi::msg::SYS_PROPERTY_INFO, 1, hfi::sys_property::IMAGE_VERSION];
                let mut text = [0u8; 128];
                let v = b"14:VIDEO.VE.5.4-00032-PROD-1";
                text[..v.len()].copy_from_slice(v);
                m.extend(text.chunks(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])));
                put_message(st, &m);
            }
            hfi::cmd::SYS_PING => put_message(st, &[12, hfi::msg::SYS_PING_ACK, pkt[2]]),
            _ => session_firmware(st, &pkt),
        }
    }
}

impl RegIo for Regs {
    fn read32(&self, off: u32) -> u32 {
        let st = self.0.borrow();
        if off == regs::CPU_CS_SCIACMDARG0 {
            return 1;
        }
        if off == regs::WRAPPER_INTR_STATUS {
            return if st.pending { regs::WRAPPER_INTR_STATUS_A2H } else { 0 };
        }
        st.regs[off as usize / 4]
    }
    fn write32(&self, off: u32, v: u32) {
        let mut st = self.0.borrow_mut();
        st.regs[off as usize / 4] = v;
        if off == regs::CPU_IC_SOFTINT {
            firmware(&mut st);
        }
        if off == regs::WRAPPER_INTR_CLEAR {
            st.pending = false;
        }
    }
}

struct NoTime;

impl Hal for NoTime {
    fn now_us(&self) -> u64 {
        0
    }
    fn delay_us(&self, _us: u32) {}
    fn log(&self, _args: std::fmt::Arguments) {}
}

/// A hal whose clock advances on every delay, so a command the model never answers times out instead of hanging.
struct Ticking(std::cell::Cell<u64>);

impl Hal for Ticking {
    fn now_us(&self) -> u64 {
        self.0.get()
    }
    fn delay_us(&self, us: u32) {
        self.0.set(self.0.get() + us as u64);
    }
    fn log(&self, _args: std::fmt::Arguments) {}
}

/// A core that has run and answered its init, with its state, for the tests below. `init_error` and `start_error` are what the
/// model puts in its SYS_SESSION_INIT and SESSION_START answers.
fn started(init_error: u32, start_error: u32) -> (S, Core<Regs, Mem>) {
    let st: S = Rc::new(RefCell::new(State {
        mem: vec![0; iface::SHARED_SIZE as usize / 4],
        regs: vec![0; 0x100000 / 4],
        pending: false,
        out: String::new(),
        init_error,
        start_error,
        release_error: 0,
        abort_answered: true,
        seq_events: Vec::new(),
        etbs: 0,
    }));
    let i = Iface::init(Mem(st.clone()), DEVICE);
    let mut core = Core::new(Regs(st.clone()), i);
    core.run(&NoTime).unwrap();
    for p in hfi::core_init() {
        core.send(&p, false).unwrap();
    }
    let mut buf = [0u32; MAX_PACKET_WORDS as usize];
    while st.borrow().pending {
        core.acknowledge();
        core.messages(&mut buf, |_| {}, |_| {}).unwrap();
    }
    (st, core)
}

/// A machine that records every allocation, every free, every quarantine, and every codec power change.
struct Recording {
    allocs: Vec<u32>,
    freed: Vec<u32>,
    quarantined: Vec<u32>,
    power: Vec<bool>,
}

impl Recording {
    fn new() -> Self {
        Recording { allocs: Vec::new(), freed: Vec::new(), quarantined: Vec::new(), power: Vec::new() }
    }
}

impl Machine for Recording {
    fn alloc(&mut self, _bytes: u32) -> Result<u32> {
        let a = 0x9000_0000 + self.allocs.len() as u32 * 0x40_0000;
        self.allocs.push(a);
        Ok(a)
    }
    fn free(&mut self, address: u32) {
        self.freed.push(address);
    }
    fn quarantine(&mut self, address: u32) {
        self.quarantined.push(address);
    }
    fn codec_power(&mut self, on: bool) -> Result<()> {
        self.power.push(on);
        Ok(())
    }
    fn load(&mut self, _mbs: u32, _bytes: u32) -> Result<()> {
        Ok(())
    }
}

/// Every address in `allocs` was freed exactly once.
fn all_freed_once(m: &Recording, allocs: &[u32]) {
    for a in allocs {
        assert_eq!(m.freed.iter().filter(|&&f| f == *a).count(), 1, "address {:#x} was not freed exactly once: {:?}", a, m.freed);
    }
}

/// Two sessions one after the other: each close gives back every address that session was given, and the codec power is off; the
/// second session's SYS_SESSION_INIT carries its own id.
#[test]
fn two_sessions_in_a_row_give_everything_back() {
    let (st, mut core) = started(0, 0);
    let mut m = Recording::new();
    let s = Session::open(&mut core, &NoTime, &mut m, 0x5e55, session::codec::H264, 1920, 1080, 4, (5, 4, 32)).unwrap();
    let first = m.allocs.clone();
    assert!(!first.is_empty(), "the first session allocated nothing");
    s.close(&mut core, &NoTime, &mut m).unwrap();
    all_freed_once(&m, &first);
    assert_eq!(m.power.last(), Some(&false), "the codec power was left on after the first close: {:?}", m.power);
    let s = Session::open(&mut core, &NoTime, &mut m, 0x1234, session::codec::H264, 1920, 1080, 4, (5, 4, 32)).unwrap();
    let second = m.allocs[first.len()..].to_vec();
    assert!(!second.is_empty(), "the second session allocated nothing");
    s.close(&mut core, &NoTime, &mut m).unwrap();
    all_freed_once(&m, &second);
    assert_eq!(m.power.last(), Some(&false), "the codec power was left on after the second close: {:?}", m.power);
    let out = st.borrow().out.clone();
    let inits: Vec<&str> = out.lines().filter(|l| l.starts_with("cmd 14 10007 ")).collect();
    assert_eq!(inits.len(), 2, "expected two SYS_SESSION_INIT packets:\n{}", out);
    assert!(inits[0].starts_with("cmd 14 10007 5e55 "), "the first init carries the wrong id: {}", inits[0]);
    assert!(inits[1].starts_with("cmd 14 10007 1234 "), "the second init does not carry the new id: {}", inits[1]);
}

/// A SYS_SESSION_INIT the firmware refuses: `open` fails, nothing was allocated, and the codec power was never on.
#[test]
fn a_refused_session_init_leaks_nothing() {
    let (_st, mut core) = started(0x1001, 0);
    let mut m = Recording::new();
    let r = Session::open(&mut core, &NoTime, &mut m, 0x5e55, session::codec::H264, 1920, 1080, 4, (5, 4, 32));
    assert!(r.is_err(), "a refused session init opened: {:?}", r.map(|s| s.id));
    assert!(m.allocs.is_empty(), "a refused session init allocated: {:?}", m.allocs);
    assert!(!m.power.contains(&true), "the codec power was left on: {:?}", m.power);
}

/// A SESSION_START the firmware refuses: `open` fails, a SYS_SESSION_ABORT went out after the start, every allocation was
/// freed, and the codec power is off.
#[test]
fn a_refused_start_aborts_and_frees() {
    let (st, mut core) = started(0, 0x1001);
    let mut m = Recording::new();
    let r = Session::open(&mut core, &NoTime, &mut m, 0x5e55, session::codec::H264, 1920, 1080, 4, (5, 4, 32));
    assert!(r.is_err(), "a refused start opened: {:?}", r.map(|s| s.id));
    let out = st.borrow().out.clone();
    let cmds: Vec<&str> = out.lines().filter(|l| l.starts_with("cmd ")).collect();
    let start = cmds.iter().position(|l| l.starts_with("cmd c 211002 "));
    let abort = cmds.iter().position(|l| l.starts_with("cmd c 210001 "));
    assert!(start.is_some(), "no SESSION_START was sent:\n{}", cmds.join("\n"));
    assert!(abort.is_some(), "no SYS_SESSION_ABORT was sent:\n{}", cmds.join("\n"));
    assert!(abort.unwrap() > start.unwrap(), "the abort went out before the start:\n{}", cmds.join("\n"));
    assert!(!m.allocs.is_empty(), "nothing was allocated before the refused start");
    all_freed_once(&m, &m.allocs.clone());
    assert_eq!(m.power.last(), Some(&false), "the codec power was left on: {:?}", m.power);
}

/// A picture size past the decoder's limit: `open` fails before any packet reaches the command queue.
#[test]
fn a_picture_size_out_of_range_is_refused_before_anything() {
    let (st, mut core) = started(0, 0);
    let before = st.borrow().out.lines().count();
    let mut m = Recording::new();
    let r = Session::open(&mut core, &NoTime, &mut m, 0x5e55, session::codec::H264, 8192, 8192, 4, (5, 4, 32));
    assert!(r.is_err(), "an out-of-range picture size opened: {:?}", r.map(|s| s.id));
    let out = st.borrow().out.clone();
    let cmds: Vec<&str> = out.lines().skip(before).filter(|l| l.starts_with("cmd ")).collect();
    assert!(cmds.is_empty(), "a packet reached the command queue:\n{}", cmds.join("\n"));
    assert!(m.allocs.is_empty() && m.power.is_empty(), "the machine was touched: {:?} {:?}", m.allocs, m.power);
}

/// A SESSION_RELEASE_RESOURCES the firmware refuses and a SYS_SESSION_ABORT it never answers: `close` fails, no internal or
/// reference buffer is freed (the block has no IOMMU, so a late firmware write would hit reused memory), every allocation is
/// quarantined instead, and the codec power is still turned off.
#[test]
fn an_unanswered_abort_quarantines_instead_of_freeing() {
    let (st, mut core) = started(0, 0);
    st.borrow_mut().release_error = 0x1001;
    st.borrow_mut().abort_answered = false;
    let mut m = Recording::new();
    let s = Session::open(&mut core, &NoTime, &mut m, 0x5e55, session::codec::H264, 1920, 1080, 4, (5, 4, 32)).unwrap();
    let all = m.allocs.clone();
    assert!(!all.is_empty(), "the session allocated nothing");
    let r = s.close(&mut core, &Ticking(std::cell::Cell::new(0)), &mut m);
    assert!(r.is_err(), "a refused release closed cleanly: {:?}", r);
    assert!(m.freed.is_empty(), "a buffer was freed although the abort was never answered: {:?}", m.freed);
    assert_eq!(m.quarantined.len(), all.len(), "not every allocation was quarantined: {:?} vs {:?}", m.quarantined, all);
    assert_eq!(m.power.last(), Some(&false), "the codec power was left on: {:?}", m.power);
}

/// A SESSION_RELEASE_RESOURCES the firmware refuses but a SYS_SESSION_ABORT it answers: `close` fails, and every internal and
/// reference buffer is freed exactly once, as before.
#[test]
fn an_answered_abort_frees_the_buffers() {
    let (st, mut core) = started(0, 0);
    st.borrow_mut().release_error = 0x1001;
    let mut m = Recording::new();
    let s = Session::open(&mut core, &NoTime, &mut m, 0x5e55, session::codec::H264, 1920, 1080, 4, (5, 4, 32)).unwrap();
    let all = m.allocs.clone();
    assert!(!all.is_empty(), "the session allocated nothing");
    let r = s.close(&mut core, &NoTime, &mut m);
    assert!(r.is_err(), "a refused release closed cleanly: {:?}", r);
    all_freed_once(&m, &all);
    assert!(m.quarantined.is_empty(), "a buffer was quarantined although the abort was answered: {:?}", m.quarantined);
    assert_eq!(m.power.last(), Some(&false), "the codec power was left on: {:?}", m.power);
}

/// What came since the last command, as `tests/oracle.rs`'s `drain` takes it.
fn drain(st: &S, core: &mut Core<Regs, Mem>, s: &mut Session) -> Vec<Event> {
    let mut all = Vec::new();
    while st.borrow().pending || s.has_events() || s.flush_pending() {
        if let Some(e) = s.poll(core, &NoTime).unwrap() {
            all.push(e);
        }
    }
    all
}

fn input(tag: u32, len: u32, ts: u64) -> session::Input {
    session::Input { time_us: ts, flags: 0, offset: 0, alloc_len: 1568768, filled_len: len, tag, address: 0x9000_0000 + tag * 0x40_0000 }
}

fn the_sequence(events: &[Event]) -> session::Sequence {
    events.iter().find_map(|e| if let Event::Sequence(q) = e { Some(*q) } else { None }).unwrap()
}

/// A 16384 x 16384 sequence-changed event with the first input: `poll` gives the event, `reconfigure` refuses it before
/// allocating anything, and `close` still frees everything once and turns the codec power off.
#[test]
fn a_hostile_size_in_the_first_sequence_event_allocates_nothing() {
    let (st, mut core) = started(0, 0);
    st.borrow_mut().seq_events = vec![(16384, 16384)];
    let mut m = Recording::new();
    let mut s = Session::open(&mut core, &NoTime, &mut m, 0x5e55, session::codec::H264, 1920, 1080, 4, (5, 4, 32)).unwrap();
    let before = m.allocs.len();
    s.empty(&mut core, &mut m, &input(0, 40000, 0)).unwrap();
    let seq = the_sequence(&drain(&st, &mut core, &mut s));
    assert_eq!((seq.width, seq.height), (16384, 16384));
    assert!(s.reconfigure(&mut core, &NoTime, &mut m, &seq).is_err(), "a 16384 x 16384 sequence was reconfigured");
    assert_eq!(m.allocs.len(), before, "the hostile sequence event made the machine allocate: {:?}", m.allocs);
    let all = m.allocs.clone();
    s.close(&mut core, &NoTime, &mut m).unwrap();
    all_freed_once(&m, &all);
    assert_eq!(m.power.last(), Some(&false), "the codec power was left on: {:?}", m.power);
}

/// A 1920 x 1080 sequence event reconfigures fine; an 8192 x 8192 one mid-stream is refused by `change_resolution` without
/// any new allocation, and `close` frees every allocation exactly once.
#[test]
fn a_hostile_size_mid_stream_allocates_nothing_new() {
    let (st, mut core) = started(0, 0);
    st.borrow_mut().seq_events = vec![(1920, 1080), (8192, 8192)];
    let mut m = Recording::new();
    let mut s = Session::open(&mut core, &NoTime, &mut m, 0x5e55, session::codec::H264, 1920, 1080, 4, (5, 4, 32)).unwrap();
    s.empty(&mut core, &mut m, &input(0, 40000, 0)).unwrap();
    let seq = the_sequence(&drain(&st, &mut core, &mut s));
    s.reconfigure(&mut core, &NoTime, &mut m, &seq).unwrap();
    drain(&st, &mut core, &mut s);
    let before = m.allocs.len();
    s.empty(&mut core, &mut m, &input(1, 9000, 40000)).unwrap();
    let seq2 = the_sequence(&drain(&st, &mut core, &mut s));
    assert_eq!((seq2.width, seq2.height), (8192, 8192));
    assert!(s.change_resolution(&mut core, &NoTime, &mut m, &seq2).is_err(), "an 8192 x 8192 resolution change went through");
    assert_eq!(m.allocs.len(), before, "the hostile mid-stream event made the machine allocate: {:?}", m.allocs);
    let all = m.allocs.clone();
    s.close(&mut core, &NoTime, &mut m).unwrap();
    all_freed_once(&m, &all);
    assert_eq!(m.power.last(), Some(&false), "the codec power was left on: {:?}", m.power);
}
