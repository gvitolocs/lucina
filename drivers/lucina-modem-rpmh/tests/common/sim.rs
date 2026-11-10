//! A software model of one RSC DRV, layout 2.7, written from what the registers mean and not from the driver.
//!
//! It knows the register file (DRV registers, the IRQ registers at the TCS base, and per TCS: control, command
//! enable, wait-for-completion, status and 16 commands of msgid/addr/data/status) and a few rules the hardware
//! documentation in Linux's comments gives:
//!
//! * the AMC mode must be enabled (CONTROL bit 16) before the trigger (bit 24) is raised, in an earlier write;
//! * to clear, the trigger goes first and the enable after, never both in one write;
//! * a trigger needs at least one enabled command; every enabled command needs a non-zero address and a MSGID that
//!   says "write" and length 8; commands written for this transfer must all be enabled and the enabled ones must
//!   all have been written for this transfer (no stale ones);
//! * command registers cannot be written while the TCS is triggered; a TCS in flight cannot be triggered again;
//! * two TCSes in flight may not hold commands to the same address (or to the same VRM resource);
//! * a triggered TCS completes some reads of `IRQ_STATUS` later: it sets its status bit, and each command's status
//!   gets ISSUED (bit 8) and, if the command asked for a response, COMPL (bit 16);
//! * the status bit is cleared by writing its bit to `IRQ_CLEAR`.
//!
//! Every violation is recorded in `errors` and the access goes on, so a test can say what was wrong. All offsets are
//! literals on purpose: nothing here is taken from the crate under test.

use std::cell::RefCell;

use lucina_qcom_soc::io::RegIo;

#[derive(Clone, Default)]
struct Tcs {
    control: u32,
    cmd_enable: u32,
    wait_for_cmpl: u32,
    msgid: [u32; 16],
    addr: [u32; 16],
    data: [u32; 16],
    status: [u32; 16],
    written: u32,
    triggered: bool,
    done: bool,
    countdown: u32,
}

/// One transfer the model started: the TCS and its commands as (msgid, addr, data).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Exec {
    pub tcs: usize,
    pub cmds: Vec<(u32, u32, u32)>,
}

pub struct State {
    pub id: u32,
    pub config: u32,
    pub tcs_base: u32,
    pub window: u32,
    pub irq_enable: u32,
    pub irq_status: u32,
    tcs: Vec<Tcs>,
    pub errors: Vec<String>,
    pub executed: Vec<Exec>,
    /// Reads of `IRQ_STATUS` between a trigger and the completion.
    pub latency: u32,
    /// Never complete.
    pub stuck: bool,
    /// Set COMPL for commands that asked for a response.
    pub ack_resp: bool,
    /// Latch a status bit only if the TCS's `IRQ_ENABLE` bit is set.
    pub status_needs_enable: bool,
    /// Make `CONTROL` read back differently (to test the read-back wait): the number of reads that return 0 first.
    pub control_lag: u32,
}

pub struct SimRsc {
    pub s: RefCell<State>,
}

const TRIG: u32 = 1 << 24;
const EN: u32 = 1 << 16;

fn same_resource(a: u32, b: u32) -> bool {
    a == b || ((a >> 16) & 0xf == 4 && (a >> 4) & 0xffff == (b >> 4) & 0xffff)
}

impl SimRsc {
    /// A DRV with `ntcs` TCSes, `ncpt` commands each, RSC_DRV_ID version `ver` (major.minor in bits 23:16 and 15:8),
    /// DRV number `drv`, TCS registers at `tcs_base` inside a window of `window` bytes.
    pub fn new(ver: u32, drv: u32, ntcs: u32, ncpt: u32, tcs_base: u32, window: u32) -> Self {
        SimRsc {
            s: RefCell::new(State {
                id: ver,
                config: (ncpt << 27) | (ntcs << (6 * drv)),
                tcs_base,
                window,
                irq_enable: 0,
                irq_status: 0,
                tcs: vec![Tcs::default(); ntcs as usize],
                errors: Vec::new(),
                executed: Vec::new(),
                latency: 1,
                stuck: false,
                ack_resp: true,
                status_needs_enable: false,
                control_lag: 0,
            }),
        }
    }

    /// DRV 2 of the Galaxy Book Go: 8 TCSes, 3 commands each, TCS base 0xd00, 64 KiB window.
    pub fn galaxy_book_go() -> Self {
        Self::new(0x0002_0700, 2, 8, 3, 0xd00, 0x10000)
    }

    pub fn errors(&self) -> Vec<String> {
        self.s.borrow().errors.clone()
    }
    pub fn assert_clean(&self) {
        let e = self.errors();
        assert!(e.is_empty(), "the RSC model saw: {e:#?}");
    }
    pub fn executed(&self) -> Vec<Exec> {
        self.s.borrow().executed.clone()
    }
    pub fn irq_enable(&self) -> u32 {
        self.s.borrow().irq_enable
    }
    pub fn irq_status(&self) -> u32 {
        self.s.borrow().irq_status
    }
    /// Is the TCS triggered (its trigger bit set)?
    pub fn triggered(&self, tcs: usize) -> bool {
        self.s.borrow().tcs[tcs].control & TRIG != 0
    }
    pub fn cmd_enable(&self, tcs: usize) -> u32 {
        self.s.borrow().tcs[tcs].cmd_enable
    }
    pub fn wait_for_cmpl(&self, tcs: usize) -> u32 {
        self.s.borrow().tcs[tcs].wait_for_cmpl
    }
    /// Let every TCS in flight complete now.
    pub fn complete_all(&self) {
        let mut s = self.s.borrow_mut();
        for t in 0..s.tcs.len() {
            if s.tcs[t].triggered && !s.tcs[t].done {
                s.complete(t);
            }
        }
    }
    pub fn set(&self, f: impl FnOnce(&mut State)) {
        f(&mut self.s.borrow_mut());
    }
    /// Plant stale commands in a TCS, as a solver or an earlier owner would have left them.
    pub fn plant_commands(&self, tcs: usize, mask: u32) {
        self.s.borrow_mut().tcs[tcs].cmd_enable = mask;
    }
}

impl State {
    fn err(&mut self, msg: String) {
        self.errors.push(msg);
    }

    fn complete(&mut self, t: usize) {
        let ack = self.ack_resp;
        let latch = !self.status_needs_enable || self.irq_enable & (1 << t) != 0;
        let tcs = &mut self.tcs[t];
        tcs.done = true;
        for j in 0..16 {
            if tcs.cmd_enable & (1 << j) != 0 {
                tcs.status[j] = 1 << 8;
                if tcs.msgid[j] & (1 << 8) != 0 && ack {
                    tcs.status[j] |= 1 << 16;
                }
            }
        }
        if latch {
            self.irq_status |= 1 << t;
        }
    }

    fn decode(&self, off: u32) -> Option<Loc> {
        if off >= self.window {
            return None;
        }
        if off < self.tcs_base {
            return Some(match off {
                0x00 => Loc::Id,
                0x04 => Loc::Solver,
                0x0c => Loc::Config,
                _ => return None,
            });
        }
        let r = off - self.tcs_base;
        match r {
            0x00 => return Some(Loc::IrqEnable),
            0x04 => return Some(Loc::IrqStatus),
            0x08 => return Some(Loc::IrqClear),
            0x0c..=0x0f => return None,
            _ => {}
        }
        let t = (r / 672) as usize;
        let reg = r % 672;
        if t >= self.tcs.len() {
            return None;
        }
        Some(match reg {
            0x10 => Loc::Tcs(t, TcsReg::WaitForCmpl),
            0x14 => Loc::Tcs(t, TcsReg::Control),
            0x18 => Loc::Tcs(t, TcsReg::Status),
            0x1c => Loc::Tcs(t, TcsReg::CmdEnable),
            0x30..=0x16f => {
                let c = ((reg - 0x30) / 20) as usize;
                if c >= 16 {
                    return None;
                }
                match (reg - 0x30) % 20 {
                    0 => Loc::Tcs(t, TcsReg::Msgid(c)),
                    4 => Loc::Tcs(t, TcsReg::Addr(c)),
                    8 => Loc::Tcs(t, TcsReg::Data(c)),
                    12 => Loc::Tcs(t, TcsReg::CmdStatus(c)),
                    16 => Loc::Tcs(t, TcsReg::Resp(c)),
                    _ => return None,
                }
            }
            _ => return None,
        })
    }

    fn write_control(&mut self, t: usize, new: u32) {
        let old = self.tcs[t].control;
        if new & !(EN | TRIG) != 0 {
            self.err(format!("TCS {t}: reserved bits in CONTROL write {new:#x}"));
        }
        if new & TRIG != 0 && new & EN == 0 {
            self.err(format!(
                "TCS {t}: TRIGGER set without AMC mode enabled ({new:#x})"
            ));
        }
        if old & TRIG != 0 && new & TRIG == 0 && new & EN == 0 {
            self.err(format!(
                "TCS {t}: enable cleared in the same write as the trigger ({old:#x} -> {new:#x})"
            ));
        }
        let rising = new & TRIG != 0 && old & TRIG == 0;
        if new & TRIG != 0 && old & TRIG != 0 && self.tcs[t].triggered && !self.tcs[t].done {
            self.err(format!(
                "TCS {t}: triggered again while a transfer is in flight"
            ));
        }
        if rising {
            if old & EN == 0 {
                self.err(format!("TCS {t}: trigger raised without AMC mode having been enabled by an earlier write"));
            }
            self.check_trigger(t);
            let tcs = &mut self.tcs[t];
            tcs.triggered = true;
            tcs.done = false;
            tcs.countdown = self.latency.max(1);
            let cmds = (0..16)
                .filter(|j| tcs.cmd_enable & (1 << j) != 0)
                .map(|j| (tcs.msgid[j], tcs.addr[j], tcs.data[j]))
                .collect();
            tcs.written = 0;
            self.executed.push(Exec { tcs: t, cmds });
        }
        if new & TRIG == 0 && old & TRIG != 0 {
            self.tcs[t].triggered = false;
        }
        self.tcs[t].control = new;
    }

    fn check_trigger(&mut self, t: usize) {
        let tcs = self.tcs[t].clone();
        if tcs.cmd_enable == 0 {
            self.err(format!("TCS {t}: trigger with no command enabled"));
        }
        if tcs.written & !tcs.cmd_enable != 0 {
            self.err(format!(
                "TCS {t}: commands {:#x} were written but not enabled",
                tcs.written & !tcs.cmd_enable
            ));
        }
        if tcs.cmd_enable & !tcs.written != 0 {
            self.err(format!(
                "TCS {t}: commands {:#x} are enabled but were not written for this transfer",
                tcs.cmd_enable & !tcs.written
            ));
        }
        for j in 0..16 {
            if tcs.cmd_enable & (1 << j) == 0 {
                continue;
            }
            if tcs.addr[j] == 0 {
                self.err(format!("TCS {t}: command {j} has address 0"));
            }
            if tcs.msgid[j] & 0xff != 8 || tcs.msgid[j] & (1 << 16) == 0 {
                self.err(format!(
                    "TCS {t}: command {j} MSGID {:#x} is not a write of length 8",
                    tcs.msgid[j]
                ));
            }
            for (u, other) in self.tcs.iter().enumerate() {
                if u == t || !other.triggered || other.done {
                    continue;
                }
                for k in 0..16 {
                    if other.cmd_enable & (1 << k) != 0 && same_resource(other.addr[k], tcs.addr[j])
                    {
                        let msg = format!(
                            "TCS {t} command {j} goes to {:#x}, which TCS {u} has in flight",
                            tcs.addr[j]
                        );
                        self.errors.push(msg);
                    }
                }
            }
        }
    }
}

enum TcsReg {
    WaitForCmpl,
    Control,
    Status,
    CmdEnable,
    Msgid(usize),
    Addr(usize),
    Data(usize),
    CmdStatus(usize),
    Resp(usize),
}

enum Loc {
    Id,
    Solver,
    Config,
    IrqEnable,
    IrqStatus,
    IrqClear,
    Tcs(usize, TcsReg),
}

impl RegIo for SimRsc {
    fn read32(&self, off: u32) -> u32 {
        let mut s = self.s.borrow_mut();
        let Some(loc) = s.decode(off) else {
            s.err(format!("read of unmodelled offset {off:#x}"));
            return 0;
        };
        match loc {
            Loc::Id => s.id,
            Loc::Solver => 0,
            Loc::Config => s.config,
            Loc::IrqEnable => s.irq_enable,
            Loc::IrqStatus => {
                // time passes while the driver looks
                for t in 0..s.tcs.len() {
                    if s.tcs[t].triggered && !s.tcs[t].done && !s.stuck {
                        s.tcs[t].countdown = s.tcs[t].countdown.saturating_sub(1);
                        if s.tcs[t].countdown == 0 {
                            s.complete(t);
                        }
                    }
                }
                s.irq_status
            }
            Loc::IrqClear => {
                s.err("read of IRQ_CLEAR".into());
                0
            }
            Loc::Tcs(t, reg) => match reg {
                TcsReg::WaitForCmpl => s.tcs[t].wait_for_cmpl,
                TcsReg::Control => {
                    if s.control_lag > 0 {
                        s.control_lag -= 1;
                        return 0xdead_0000;
                    }
                    s.tcs[t].control
                }
                TcsReg::Status => u32::from(!s.tcs[t].triggered || s.tcs[t].done),
                TcsReg::CmdEnable => s.tcs[t].cmd_enable,
                TcsReg::Msgid(c) => s.tcs[t].msgid[c],
                TcsReg::Addr(c) => s.tcs[t].addr[c],
                TcsReg::Data(c) => s.tcs[t].data[c],
                TcsReg::CmdStatus(c) => s.tcs[t].status[c],
                TcsReg::Resp(_) => 0,
            },
        }
    }

    fn write32(&self, off: u32, value: u32) {
        let mut s = self.s.borrow_mut();
        let Some(loc) = s.decode(off) else {
            s.err(format!("write of {value:#x} to unmodelled offset {off:#x}"));
            return;
        };
        match loc {
            Loc::Id | Loc::Solver | Loc::Config => {
                s.err(format!("write to read-only DRV register {off:#x}"))
            }
            Loc::IrqEnable => s.irq_enable = value,
            Loc::IrqStatus => s.err("write to IRQ_STATUS".into()),
            Loc::IrqClear => s.irq_status &= !value,
            Loc::Tcs(t, reg) => match reg {
                TcsReg::Status | TcsReg::CmdStatus(_) | TcsReg::Resp(_) => {
                    s.err(format!("TCS {t}: write to read-only register {off:#x}"))
                }
                TcsReg::WaitForCmpl => s.tcs[t].wait_for_cmpl = value,
                TcsReg::Control => s.write_control(t, value),
                TcsReg::CmdEnable => {
                    if s.tcs[t].control & TRIG != 0 && value != 0 {
                        s.err(format!(
                            "TCS {t}: CMD_ENABLE written with commands while triggered"
                        ));
                    }
                    s.tcs[t].cmd_enable = value;
                }
                TcsReg::Msgid(c) | TcsReg::Addr(c) | TcsReg::Data(c) => {
                    if s.tcs[t].control & TRIG != 0 {
                        s.err(format!(
                            "TCS {t}: command {c} written while the TCS is triggered"
                        ));
                    }
                    s.tcs[t].written |= 1 << c;
                    match reg {
                        TcsReg::Msgid(_) => s.tcs[t].msgid[c] = value,
                        TcsReg::Addr(_) => s.tcs[t].addr[c] = value,
                        _ => s.tcs[t].data[c] = value,
                    }
                }
            },
        }
    }
}
