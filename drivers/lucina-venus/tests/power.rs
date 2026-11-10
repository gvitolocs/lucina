//! The crate's suspend and resume against Linux's own: `oracle/power.c` runs hfi_venus.c's `venus_suspend_3xx` and `venus_resume`
//! against a model of the block, and `fixtures/power.txt` is what that printed from its "power" line on. This runs
//! [`Core::suspend`] and [`Core::resume`] against the same registers and must print the same lines: every register write, the
//! command packet, and the secure world's calls, in order.

use std::cell::RefCell;
use std::fmt::Write;
use std::rc::Rc;

use lucina_qcom_soc::io::RegIo;
use lucina_venus::{
    Hal, Result,
    cpu::Core,
    iface::{self, Iface, Shared},
    power::RemoteState,
    regs,
};

const DEVICE: u32 = 0xd000_0000;

struct State {
    mem: Vec<u32>,
    regs: Vec<u32>,
    out: String,
    suspending: bool,
    pc_prep_sent: bool,
    /// What the registers say when the processor never goes idle.
    never_idle: bool,
}

type S = Rc<RefCell<State>>;

fn value(v: u32) -> String {
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

/// The oracle's `firmware()` for what this test sends: every command printed and taken off the queue, none answered.
fn firmware(st: &mut State) {
    let h = 6;
    let ring = ((st.mem[h + 1] - DEVICE) / 4) as usize;
    let size = st.mem[h + 3] as usize;
    while st.mem[h + 12] != st.mem[h + 13] {
        let rd = st.mem[h + 12] as usize;
        let n = (st.mem[ring + rd] / 4) as usize;
        let pkt: Vec<u32> = (0..n).map(|k| st.mem[ring + (rd + k) % size]).collect();
        let _ = writeln!(st.out, "cmd {}", pkt.iter().map(|w| format!("{:x}", w)).collect::<Vec<_>>().join(" "));
        st.mem[h + 12] = ((rd + n) % size) as u32;
    }
}

struct Regs(S);

impl RegIo for Regs {
    fn read32(&self, off: u32) -> u32 {
        let st = self.0.borrow();
        if st.suspending {
            if off == regs::WRAPPER_CPU_STATUS {
                return if st.never_idle { 0 } else { regs::WRAPPER_CPU_STATUS_WFI };
            }
            if off == regs::CPU_CS_SCIACMDARG0 {
                return 1 | regs::SCIACMDARG0_INIT_IDLE_MSG | if st.pc_prep_sent { regs::SCIACMDARG0_PC_READY } else { 0 };
            }
            if off == regs::WRAPPER_CPU_AXI_HALT_STATUS {
                return 1 << 24;
            }
        }
        if off == regs::CPU_CS_SCIACMDARG0 {
            return 1;
        }
        st.regs[off as usize / 4]
    }
    fn write32(&self, off: u32, v: u32) {
        let mut st = self.0.borrow_mut();
        let _ = writeln!(st.out, "w {:x} {}", off, value(v));
        st.regs[off as usize / 4] = v;
        if off == regs::CPU_IC_SOFTINT {
            firmware(&mut st);
            if st.suspending {
                st.pc_prep_sent = true;
            }
        }
    }
}

struct Tz(S);

impl RemoteState for Tz {
    fn set_remote_state(&mut self, resume: bool) -> Result<()> {
        let _ = writeln!(self.0.borrow_mut().out, "scm set_remote_state {}", resume as u32);
        Ok(())
    }
}

/// Time that moves only when the driver waits, so that a timeout is reached without waiting for it.
struct Clock(RefCell<u64>);

impl Hal for Clock {
    fn now_us(&self) -> u64 {
        *self.0.borrow()
    }
    fn delay_us(&self, us: u32) {
        *self.0.borrow_mut() += u64::from(us);
    }
    fn log(&self, _args: std::fmt::Arguments) {}
}

fn started(never_idle: bool) -> (S, Core<Regs, Mem>) {
    let st: S = Rc::new(RefCell::new(State {
        mem: vec![0; iface::SHARED_SIZE as usize / 4],
        regs: vec![0; 0x100000 / 4],
        out: String::new(),
        suspending: false,
        pc_prep_sent: false,
        never_idle,
    }));
    let core = Core::new(Regs(st.clone()), Iface::init(Mem(st.clone()), DEVICE));
    core.run(&Clock(RefCell::new(0))).unwrap();
    st.borrow_mut().out.clear();
    (st, core)
}

#[test]
fn suspend_and_resume_are_linux() {
    let (st, core) = started(false);
    let hal = Clock(RefCell::new(0));
    st.borrow_mut().out.push_str("power\n");
    st.borrow_mut().suspending = true;
    let r = core.suspend(&hal, &mut Tz(st.clone()));
    let _ = writeln!(st.borrow_mut().out, "suspend {}", if r.is_ok() { 0 } else { -1 });
    st.borrow_mut().suspending = false;
    let r = core.resume(&hal, &mut Tz(st.clone()));
    let _ = writeln!(st.borrow_mut().out, "resume {}", if r.is_ok() { 0 } else { -1 });
    let ours: Vec<String> = st.borrow().out.lines().map(String::from).collect();
    let linux: Vec<&str> = include_str!("fixtures/power.txt").lines().collect();
    for (i, (a, b)) in ours.iter().zip(&linux).enumerate() {
        assert_eq!(a, b, "line {} differs", i + 1);
    }
    assert_eq!(ours.len(), linux.len(), "ours:\n{}", ours.join("\n"));
}

#[test]
fn a_processor_that_never_goes_idle_is_not_powered_off() {
    let (st, core) = started(true);
    let hal = Clock(RefCell::new(0));
    st.borrow_mut().suspending = true;
    let r = core.suspend(&hal, &mut Tz(st.clone()));
    assert!(r.is_err());
    let out = st.borrow().out.clone();
    assert!(!out.contains("set_remote_state"), "the secure world was told to suspend a busy processor:\n{out}");
    assert!(!out.contains("cmd "), "SYS_PC_PREP was sent to a busy processor:\n{out}");
    assert!(!out.contains(&format!("w {:x} ", regs::WRAPPER_CPU_AXI_HALT)), "the bus was halted:\n{out}");
    assert!(*hal.0.borrow() >= lucina_venus::power::IDLE_TIMEOUT_US, "gave up before Linux's 150 ms");
}
