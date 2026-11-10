//! The crate against Linux's own code: `oracle/` compiles hfi_venus.c, hfi_cmds.c and hfi_msgs.c unchanged and drives them as core.c
//! does, against a model of the block and its firmware, and `fixtures/oracle.txt` is what that printed. This runs the same steps with
//! the same model through the crate and must print the same lines: every register write in order, the queue table, every command
//! packet word for word, and what came of the firmware's answers.

use std::cell::RefCell;
use std::fmt::Write;
use std::rc::Rc;

use lucina_qcom_soc::io::RegIo;
use lucina_venus::{
    Hal, Result,
    cpu::{self, Core},
    decode::{Event, Machine, Session},
    hfi::{self, Message},
    iface::{self, Iface, Shared},
    regs,
    rules::MAX_PACKET_WORDS,
    session::{self, Input, buffer},
};

const DEVICE: u32 = 0xd000_0000;

struct State {
    mem: Vec<u32>,
    regs: Vec<u32>,
    pending: bool,
    out: String,
    event_sent: bool,
    second_event_sent: bool,
    inputs_seen: u32,
    frame_no: u32,
}

type S = Rc<RefCell<State>>;

fn value(v: u32) -> String {
    // The SFR sits at table+0x259000 in both: inside the region here, and where the oracle's allocator puts Linux's second allocation.
    if (DEVICE..DEVICE + iface::SHARED_SIZE).contains(&v) {
        return format!("table+{:x}", v - DEVICE);
    }
    format!("{:x}", v)
}

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


/// The oracle's requirements table: type, size, region size, count_min (hold_count on 4xx), count_min_host, count_actual, contiguous,
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

/// The requirements after the resolution change to 1280 x 720: the same counts, the smaller sizes.
const BUFREQ_720: [[u32; 8]; 8] = [
    [1, 1568768, 0, 2, 2, 4, 1, 4096],
    [2, 1507328, 0, 6, 6, 8, 1, 4096],
    [3, 1413120, 0, 6, 6, 8, 1, 4096],
    [6, 0x300000, 0, 1, 1, 1, 1, 4096],
    [7, 0x80000, 0, 1, 1, 1, 1, 4096],
    [8, 0, 0, 0, 0, 0, 1, 4096],
    [4, 0x100000, 0, 1, 1, 1, 1, 4096],
    [5, 0x40000, 0, 1, 1, 1, 1, 4096],
];

fn bufreq_message(sid: u32, res_changed: bool) -> Vec<u32> {
    let mut m = vec![20 + 32 * 8, hfi::msg::SESSION_PROPERTY_INFO, sid, 1, session::property::BUFFER_REQUIREMENTS];
    m.extend((if res_changed { BUFREQ_720 } else { BUFREQ }).iter().flatten());
    m
}

/// The oracle's `session_firmware()`.
fn session_firmware(st: &mut State, pkt: &[u32]) {
    use hfi::{cmd, msg};
    let sid = pkt[2];
    let done = |st: &mut State, kind: u32| {
        let mut m = vec![0u32; 8];
        m[0] = 32;
        m[1] = kind;
        m[2] = sid;
        put_message(st, &m);
    };
    match pkt[1] {
        cmd::SYS_SESSION_INIT => done(st, msg::SYS_SESSION_INIT),
        cmd::SYS_SESSION_END => done(st, msg::SYS_SESSION_END),
        cmd::SESSION_LOAD_RESOURCES => done(st, msg::SESSION_LOAD_RESOURCES),
        cmd::SESSION_START => done(st, msg::SESSION_START),
        cmd::SESSION_STOP => done(st, msg::SESSION_STOP),
        cmd::SESSION_RELEASE_RESOURCES => done(st, msg::SESSION_RELEASE_RESOURCES),
        cmd::SESSION_RELEASE_BUFFERS => done(st, msg::SESSION_RELEASE_BUFFERS),
        cmd::SESSION_FLUSH => put_message(st, &[20, msg::SESSION_FLUSH, sid, 0, pkt[3]]),
        cmd::SESSION_GET_PROPERTY => put_message(st, &bufreq_message(sid, st.second_event_sent)),
        cmd::SESSION_EMPTY_BUFFER => {
            put_message(st, &[36, msg::SESSION_EMPTY_BUFFER, sid, 0, pkt[8], pkt[10], pkt[11], pkt[12], 0]);
            st.inputs_seen += 1;
            if !st.event_sent {
                st.event_sent = true;
                let mut e = vec![0, msg::EVENT_NOTIFY, sid, session::event::SEQUENCE_CHANGED, 0x100_0001, 5,
                    0x1001, 3, 1920, 1080, 0x100_3007, 3, 0, 0x100_3009, 1, 0x120_300e, 5, 4, 4, 2, 6,
                    0x0700_000e, 28, 1, 1, 0, 0, 1920, 1080];
                e[0] = (e.len() * 4) as u32;
                put_message(st, &e);
            } else if !st.second_event_sent && st.inputs_seen == 3 {
                // The resolution change: a second sequence-changed event, 1280 x 720, after the seek's input buffer.
                st.second_event_sent = true;
                let mut e = vec![0, msg::EVENT_NOTIFY, sid, session::event::SEQUENCE_CHANGED, 0x100_0001, 5,
                    0x1001, 3, 1280, 720, 0x100_3007, 3, 0, 0x100_3009, 1, 0x120_300e, 5, 4, 4, 2, 6,
                    0x0700_000e, 28, 1, 1, 0, 0, 1280, 720];
                e[0] = (e.len() * 4) as u32;
                put_message(st, &e);
            }
        }
        cmd::SESSION_FILL_BUFFER if pkt[3] == 1 => {
            st.frame_no += 1;
            let f = st.frame_no;
            put_message(st, &[100, msg::SESSION_FILL_BUFFER, sid, 1, 0, 0, 0, 40000 * f, 0, 0, 0, 0,
                pkt[5], 3133440, 0, 1920, 1088, 0, 0, 0, 0, pkt[7], 2, pkt[8], 0]);
        }
        _ => {}
    }
}

/// The oracle's `firmware()`.
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
        let _ = writeln!(st.out, "w {:x} {}", off, value(v));
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

#[test]
fn the_core_and_a_session_are_linux() {
    let st: S = Rc::new(RefCell::new(State {
        mem: vec![0; iface::SHARED_SIZE as usize / 4],
        regs: vec![0; 0x100000 / 4],
        pending: false,
        out: String::new(),
        event_sent: false,
        second_event_sent: false,
        inputs_seen: 0,
        frame_no: 0,
    }));
    let i = Iface::init(Mem(st.clone()), DEVICE);
    {
        let mut s = st.borrow_mut();
        let table: Vec<String> = s.mem[..6 + 14 * 3].iter().map(|w| value(*w)).collect();
        let line = format!("table {}\n", table.join(" "));
        s.out.push_str(&line);
        // The kernel's set_remote_state(1, 0) comes between the table and the queue registers, as venus_power_on has it.
        s.out.push_str("scm set_remote_state 1\n");
    }
    let mut core = Core::new(Regs(st.clone()), i);
    core.run(&NoTime).unwrap();
    for p in hfi::core_init() {
        core.send(&p, false).unwrap();
    }
    let mut buf = [0u32; MAX_PACKET_WORDS as usize];
    let mut error = None;
    let mut version = None;
    let mut text = [0u8; 128];
    while st.borrow().pending {
        core.acknowledge();
        core.messages(
            &mut buf,
            |m| match m.kind() {
                hfi::msg::SYS_INIT => error = Some(hfi::sys_init_done(m)),
                hfi::msg::SYS_PROPERTY_INFO => version = hfi::image_version(m, &mut text).and_then(hfi::parse_version),
                _ => {}
            },
            |_| {},
        )
        .unwrap();
    }
    let error = match error.unwrap() {
        Ok(_) => 0,
        Err(e) => e,
    };
    let (major, minor, rev) = version.unwrap();
    let _ = writeln!(st.borrow_mut().out, "core error {} version {}.{}-{}", error, major, minor, rev);
    session(&st, &mut core);
    let keep = |l: &&str| !l.starts_with("bufreq") && !l.starts_with("flushed") && !l.starts_with("error");
    let ours: Vec<String> = st.borrow().out.lines().map(String::from).collect();
    // venus_session_etb leaves the empty-buffer packet's last two words (extradata_buffer, data) unset: what the oracle prints there
    // is whatever its stack held. Ours writes 0, so those two words are compared as 0.
    let linux: Vec<String> = include_str!("fixtures/oracle.txt")
        .lines()
        .filter(keep)
        .map(|l| {
            if l.starts_with("cmd 3c 211004 ") {
                let mut w: Vec<&str> = l.split(' ').collect();
                let n = w.len();
                w[n - 2] = "0";
                w[n - 1] = "0";
                w.join(" ")
            } else {
                l.to_string()
            }
        })
        .collect();
    for (n, (a, b)) in ours.iter().zip(linux.iter()).enumerate() {
        assert_eq!(a, b, "line {} differs", n + 1);
    }
    assert_eq!(ours.len(), linux.len(), "ours:\n{}", ours.join("\n"));
    // The requirements as Linux's session_get_prop_buf_req read them, against session::requirements on the same bytes.
    let words = bufreq_message(0x5e55, false);
    let ours: Vec<String> = session::requirements(&Message { words: &words })
        .unwrap()
        .iter()
        .map(|r| format!("bufreq {} size {} min {} actual {}", r.kind, r.size, r.count_min, r.count_actual))
        .collect();
    let linux: Vec<&str> = include_str!("fixtures/oracle.txt").lines().filter(|l| l.starts_with("bufreq")).take(8).collect();
    assert_eq!(ours, linux);
    let _ = cpu::ANSWER_TIMEOUT_US;
}

/// The oracle's `address()`, in the order the session asks for memory.
struct Addresses(Vec<u32>);

fn address(kind: u32, i: u32) -> u32 {
    0x9000_0000 + kind * 0x100_0000 + i * 0x40_0000
}

impl Machine for Addresses {
    fn alloc(&mut self, _bytes: u32) -> Result<u32> {
        Ok(self.0.remove(0))
    }
    fn free(&mut self, _address: u32) {}
    fn codec_power(&mut self, _on: bool) -> Result<()> {
        Ok(())
    }
    fn load(&mut self, _mbs: u32, _bytes: u32) -> Result<()> {
        Ok(())
    }
}

/// Linux's `hfi_session_ftb_done` and `hfi_session_etb_done` as the oracle's `buf_done` prints them.
fn print_event(out: &mut String, e: &Event) {
    match e {
        Event::InputDone(d) => {
            let _ = writeln!(out, "done 1 tag {} bytes {} off {} flags 0 hfi 0 ts 0", d.tag, d.filled_len, d.offset);
        }
        Event::Picture(o) => {
            let mut flags = if o.flags & session::flag::EOS != 0 { 0x10_0000 } else { 0 };
            flags |= match o.picture_type {
                1 | 8 => 0x8,
                2 => 0x10,
                4 => 0x20,
                _ => 0,
            };
            let ts = if o.flags & session::flag::TIMESTAMPINVALID == 0 && o.filled_len != 0 { o.time_us } else { 0 };
            let _ = writeln!(out, "done 3 tag {} bytes {} off {} flags {:x} hfi {:x} ts {}", o.tag, o.filled_len, o.offset, flags, o.flags, ts);
        }
        Event::Sequence(q) => {
            let _ = writeln!(
                out,
                "event 1 type {:x} w {} h {} depth {} pic {} count {} crop {} {} {} {}",
                if q.sufficient { 0x100_0001 } else { 0x100_0002 },
                q.width,
                q.height,
                q.bit_depth,
                q.pic_struct,
                q.buffer_count,
                q.crop[0],
                q.crop[1],
                q.crop[2],
                q.crop[3]
            );
        }
        _ => {}
    }
}

/// What came since the last command, as the oracle's `run()` takes the interrupt after each.
fn drain(st: &S, core: &mut Core<Regs, Mem>, s: &mut Session) -> Vec<Event> {
    let mut all = Vec::new();
    // Only when the model raised its interrupt or events wait: poll() acknowledges the interrupt each time it reads the queues.
    while st.borrow().pending || s.has_events() || s.flush_pending() {
        if let Some(e) = s.poll(core, &NoTime).unwrap() {
            print_event(&mut st.borrow_mut().out, &e);
            all.push(e);
        }
    }
    all
}

/// The oracle's `session()`, through `decode::Session`.
fn session(st: &S, core: &mut Core<Regs, Mem>) {
    st.borrow_mut().out.push_str("session\n");
    let mut m = Addresses(vec![
        address(6, 0), address(7, 0), address(4, 0), address(5, 0),
        address(6, 0), address(7, 0),
        address(2, 32), address(2, 33), address(2, 34), address(2, 35), address(2, 36), address(2, 37),
        address(6, 0), address(7, 0),
        address(2, 32), address(2, 33), address(2, 34), address(2, 35), address(2, 36), address(2, 37),
    ]);
    let mut s = Session::open(core, &NoTime, &mut m, 0x5e55, session::codec::H264, 1920, 1080, 4, (5, 4, 32)).unwrap();
    let input = |tag: u32, len: u32, ts: u64| Input { time_us: ts, flags: 0, offset: 0, alloc_len: 1568768, filled_len: len, tag, address: address(1, tag) };
    s.empty(core, &mut m, &input(0, 40000, 0)).unwrap();
    let seq = drain(st, core, &mut s).into_iter().find_map(|e| if let Event::Sequence(q) = e { Some(q) } else { None }).unwrap();
    s.reconfigure(core, &NoTime, &mut m, &seq).unwrap();
    drain(st, core, &mut s);
    for tag in 0..2 {
        s.fill(core, tag, address(buffer::OUTPUT2, tag)).unwrap();
        drain(st, core, &mut s);
    }
    s.empty(core, &mut m, &input(1, 9000, 40000)).unwrap();
    drain(st, core, &mut s);
    // A seek: vdec_stop_output, then vdec_start_output's SEEK branch with the capture side streaming.
    s.flush_for_seek(core, &NoTime).unwrap();
    s.resume_after_seek(core).unwrap();
    for tag in 0..2 {
        s.fill(core, tag, address(buffer::OUTPUT2, tag)).unwrap();
        drain(st, core, &mut s);
    }
    s.empty(core, &mut m, &input(2, 5000, 80000)).unwrap();
    let events = drain(st, core, &mut s);
    let seq2 = events.iter().find_map(|e| if let Event::Sequence(q) = e { Some(*q) } else { None }).unwrap();
    // The second sequence-changed event put the session in DRC; vdec_stop_capture, then vdec_start_capture.
    s.change_resolution(core, &NoTime, &mut m, &seq2).unwrap();
    drain(st, core, &mut s);
    s.end_of_stream(core).unwrap();
    drain(st, core, &mut s);
    s.close(core, &NoTime, &mut m).unwrap();
}

/// A core that has run and answered its init, with its state, for the unit tests below.
fn started() -> (S, Core<Regs, Mem>) {
    let st: S = Rc::new(RefCell::new(State {
        mem: vec![0; iface::SHARED_SIZE as usize / 4],
        regs: vec![0; 0x100000 / 4],
        pending: false,
        out: String::new(),
        event_sent: false,
        second_event_sent: false,
        inputs_seen: 0,
        frame_no: 0,
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

/// A machine that records what it gave and what it took back.
struct Recording {
    allocs: Vec<u32>,
    freed: Vec<u32>,
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
    fn codec_power(&mut self, _on: bool) -> Result<()> {
        Ok(())
    }
    fn load(&mut self, _mbs: u32, _bytes: u32) -> Result<()> {
        Ok(())
    }
}

/// `flush_for_seek` before any sequence event: only the flush is sent, and the resume sends no FTB (no references yet).
#[test]
fn a_seek_before_any_sequence_event_only_flushes() {
    let (st, mut core) = started();
    let mut m = Recording { allocs: Vec::new(), freed: Vec::new() };
    let mut s = Session::open(&mut core, &NoTime, &mut m, 0x5e55, session::codec::H264, 1920, 1080, 4, (5, 4, 32)).unwrap();
    let before = st.borrow().out.lines().count();
    s.flush_for_seek(&mut core, &NoTime).unwrap();
    s.resume_after_seek(&mut core).unwrap();
    let out = st.borrow().out.clone();
    let cmds: Vec<&str> = out.lines().skip(before).filter(|l| l.starts_with("cmd")).collect();
    assert_eq!(cmds, ["cmd 10 211008 5e55 1000004"]);
}

/// `change_resolution` frees exactly the old reference pictures, and none of the new ones.
#[test]
fn a_resolution_change_frees_exactly_the_old_references() {
    let (st, mut core) = started();
    let mut m = Recording { allocs: Vec::new(), freed: Vec::new() };
    let mut s = Session::open(&mut core, &NoTime, &mut m, 0x5e55, session::codec::H264, 1920, 1080, 4, (5, 4, 32)).unwrap();
    let input = |tag: u32, len: u32, ts: u64| Input { time_us: ts, flags: 0, offset: 0, alloc_len: 1568768, filled_len: len, tag, address: address(1, tag) };
    s.empty(&mut core, &mut m, &input(0, 40000, 0)).unwrap();
    let seq = drain(&st, &mut core, &mut s).into_iter().find_map(|e| if let Event::Sequence(q) = e { Some(q) } else { None }).unwrap();
    s.reconfigure(&mut core, &NoTime, &mut m, &seq).unwrap();
    drain(&st, &mut core, &mut s);
    let old_refs: Vec<u32> = m.allocs[6..12].to_vec();
    m.freed.clear();
    // The seek's input buffer is the third: the model sends the second sequence-changed event with it.
    s.empty(&mut core, &mut m, &input(1, 9000, 40000)).unwrap();
    drain(&st, &mut core, &mut s);
    s.empty(&mut core, &mut m, &input(2, 5000, 80000)).unwrap();
    let events = drain(&st, &mut core, &mut s);
    let seq2 = events.iter().find_map(|e| if let Event::Sequence(q) = e { Some(*q) } else { None }).unwrap();
    s.change_resolution(&mut core, &NoTime, &mut m, &seq2).unwrap();
    // After the change: two scratch buffers re-allocated, then the six new reference pictures.
    let new_refs: Vec<u32> = m.allocs[14..].to_vec();
    assert_eq!(new_refs.len(), 6);
    assert!(old_refs.iter().all(|a| m.freed.contains(a)), "an old reference was not freed: {:?}", m.freed);
    assert!(!new_refs.iter().any(|a| m.freed.contains(a)), "a new reference was freed: {:?}", m.freed);
}
