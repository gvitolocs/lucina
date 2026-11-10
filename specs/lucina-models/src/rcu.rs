//! RCU grace periods across CPUs: spec `docs/spec/rcu.md`, layers (c) and (d).
//!
//! What is modelled: N CPUs, each in user mode, kernel mode, an RCU read-side section, idle, or an interrupt taken while
//! idle that reads; one published pointer that kernel code replaces, retiring the old object; the monitor of
//! `ostd/src/sync/rcu/monitor.rs` (patches 1050, 1059, 1112) with its bookkeeping `lucina_rcu::Monitor`, here as [`Mon`],
//! which `tests/rcu.rs` checks against the shipped crate step by step; the tick.
//!
//! Where the kernel reports a quiescent state (QS): a task switch, the top of the return-to-user loop
//! (`UserContext::execute`), and `halt_cpu` before it idles. Each report is `finish_grace_period`: when the monitor is
//! monitoring, `quiescent(cpu)` then `take_ready`, and the ready callbacks free their objects. It runs under the monitor's
//! lock with interrupts off, so it is one step here. The fast path reads `is_monitoring` Relaxed outside the lock; a stale
//! `false` only skips this CPU's report, which is the same as reporting a little earlier, so it is not a separate step.
//! Running the callbacks outside the lock only frees later, which no property can mind.
//!
//! The objects freed are the callbacks' work; the budgets of 1112 only matter for backlogs larger than one chunk (128), which
//! Verus covers (`verification/juno-rcu`). Here every take hands out everything ready.

use stateright::{Model, Property};

/// Whether an idle CPU still gets the periodic tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tick {
    /// Every CPU takes the tick, idle or not: an idle CPU wakes, finds nothing to run, and `halt_cpu` reports again.
    /// Juno's default today (`lucina.cpuidle` off).
    Periodic,
    /// An idle CPU's tick is stopped (`lucina.cpuidle=cpu`, 1020-1023; the Gold tick stop of juno-power step A): it wakes
    /// only for its own timer or an interrupt, which may not come for a long time.
    Tickless,
}

/// How an idle CPU takes part in grace periods.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IdleQs {
    /// Shipped (1050): `halt_cpu` reports once on the way in. A grace period that starts while the CPU is idle waits for it
    /// to wake and report again.
    OnEntry,
    /// Proposed (Linux's extended quiescent state, `ct_idle_enter`/`ct_irq_enter`): a CPU marks itself idle after its last
    /// report and counts as passed for every grace period while marked; an interrupt clears the mark before its handler can
    /// read, reports when it is done (the idle loop's `halt_cpu`) and sets it again. A grace period that a report starts
    /// counts the reporting CPU.
    Extended,
    /// Witness: the extended state, but an interrupt on an idle CPU reads without clearing the mark.
    ExtendedIrqStays,
}

#[derive(Clone, Debug)]
pub struct Rcu {
    pub cpus: usize,
    /// How many times the pointer is replaced (objects 1..=updates; object 0 is published first).
    pub updates: u8,
    pub tick: Tick,
    pub idle: IdleQs,
    /// Assumption K: a CPU in kernel mode reaches a QS within a tick (preemption at the tick, or the system call returns).
    /// `false` is the witness of a kernel loop that never switches.
    pub kernel_qs_per_tick: bool,
    /// The clock runs to here (ticks).
    pub max_time: u8,
    /// Layer (d): every retired object is freed by `retired_at + bound` ticks.
    pub bound: u8,
}

impl Rcu {
    /// The configuration that must hold for the shipped design: periodic tick, report on idle entry.
    pub fn shipped(cpus: usize) -> Self {
        Rcu {
            cpus,
            updates: 2,
            tick: Tick::Periodic,
            idle: IdleQs::OnEntry,
            kernel_qs_per_tick: true,
            max_time: 4,
            bound: 2,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Mode {
    User,
    Kernel,
    /// Inside a read-side section, holding a reference to this object.
    Read(u8),
    Idle,
    /// An interrupt taken while idle, its handler inside a read-side section.
    IrqRead(u8),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Obj {
    Unused,
    Live,
    /// Retired at this tick; its callback frees it after a grace period.
    Retired(u8),
    Freed,
}

/// `lucina_rcu::Monitor`'s state, with `Vec`s for its queues (every model backlog fits in one chunk). `tests/rcu.rs` replays
/// every short sequence of calls on both and compares what they hand out and `is_monitoring`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Mon {
    pub current: Vec<u8>,
    pub next: Vec<u8>,
    pub ready: Vec<u8>,
    pub passed: u64,
    pub complete: bool,
    pub monitoring: bool,
}

impl Mon {
    pub fn new() -> Self {
        Mon {
            complete: true,
            ..Default::default()
        }
    }

    /// `Monitor::retire`. `counted` is the mask a new grace period starts with: 0 as shipped, the idle CPUs with
    /// [`IdleQs::Extended`].
    pub fn retire(&mut self, item: u8, counted: u64) {
        self.next.push(item);
        if self.complete {
            self.current = core::mem::take(&mut self.next);
            self.passed = counted;
            self.complete = false;
        }
        self.monitoring = true;
    }

    /// `Monitor::quiescent`, with `counted` added to `passed` (0 as shipped). `restart_counts_caller`: a grace period that
    /// this call starts already has the caller passed (it is at a quiescent point, under the lock, after the start). Shipped:
    /// `false`, the caller reports again later.
    pub fn quiescent(&mut self, cpu: u32, all: u64, counted: u64, restart_counts_caller: bool) {
        if !self.monitoring || self.complete {
            return;
        }
        if cpu >= 64 || all & (1u64 << cpu) == 0 {
            return;
        }
        self.passed |= (1u64 << cpu) | (counted & all);
        if self.passed & all != all || !self.ready.is_empty() {
            return;
        }
        self.complete = true;
        core::mem::swap(&mut self.ready, &mut self.current);
        if !self.next.is_empty() {
            core::mem::swap(&mut self.current, &mut self.next);
            self.passed = (counted & all)
                | if restart_counts_caller {
                    1u64 << cpu
                } else {
                    0
                };
            self.complete = false;
        } else {
            self.monitoring = !self.ready.is_empty();
        }
    }

    /// `Monitor::take_ready`: everything ready (fewer than `CHUNK` items in every model).
    pub fn take_ready(&mut self) -> Vec<u8> {
        let out = core::mem::take(&mut self.ready);
        if self.complete && self.ready.is_empty() {
            self.monitoring = false;
        }
        out
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct S {
    pub mode: Vec<Mode>,
    /// The extended-QS mark per CPU (only with `Extended*`).
    pub eqs: Vec<bool>,
    pub ptr: u8,
    pub obj: Vec<Obj>,
    pub mon: Mon,
    pub now: u8,
    pub updates: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    /// Kernel → Read(ptr): `rcu_read_lock` and a load of the pointer.
    Read(usize),
    /// Read → Kernel.
    Unlock(usize),
    /// Kernel code publishes a new object and retires the old one (`call_rcu`/`RcuDrop`).
    Update(usize),
    /// Kernel → User: the top of the return-to-user loop reports a QS.
    ToUser(usize),
    /// User → Kernel: a system call or an interrupt in user mode.
    Enter(usize),
    /// A task switch: QS.
    Switch(usize),
    /// Kernel → Idle: `halt_cpu` reports a QS (`finish_grace_period_idle`), then idles.
    Halt(usize),
    /// Idle → Kernel: a timer or an interrupt wakes it and something runs.
    Wake(usize),
    /// Idle → IrqRead(ptr): an interrupt handler on the idle CPU reads.
    Irq(usize),
    /// IrqRead → Idle: the handler is done and the CPU idles again.
    IrqDone(usize),
    /// One tick of the clock; every CPU the tick reaches passes a QS.
    Tick,
}

impl Rcu {
    fn all(&self) -> u64 {
        if self.cpus >= 64 {
            u64::MAX
        } else {
            (1u64 << self.cpus) - 1
        }
    }

    fn extended(&self) -> bool {
        matches!(self.idle, IdleQs::Extended | IdleQs::ExtendedIrqStays)
    }

    fn counted(&self, s: &S) -> u64 {
        if !self.extended() {
            return 0;
        }
        s.eqs
            .iter()
            .enumerate()
            .filter(|(_, e)| **e)
            .fold(0, |m, (i, _)| m | (1u64 << i))
    }

    /// `finish_grace_period` on `cpu`: one step under the monitor's lock.
    fn qs(&self, s: &mut S, cpu: usize) {
        if !s.mon.monitoring {
            return;
        }
        let counted = self.counted(s);
        // Shipped: one report, one take. With the extended state the report goes round again while it changes something: a
        // grace period it starts may already be over (every other CPU idle), and once the ready items are out nobody else
        // may come to end it. The model found both holes (2026-11-08).
        let rounds = if self.extended() { 4 } else { 1 };
        for _ in 0..rounds {
            let before = s.mon.clone();
            // With the extended state, a grace period this report starts counts the reporting CPU: otherwise the last CPU
            // to go idle starts one that waits for it, and nobody reports again.
            s.mon
                .quiescent(cpu as u32, self.all(), counted, self.extended());
            for o in s.mon.take_ready() {
                s.obj[o as usize] = Obj::Freed;
            }
            if s.mon == before || !s.mon.monitoring {
                break;
            }
        }
    }
}

impl Model for Rcu {
    type State = S;
    type Action = Action;

    fn init_states(&self) -> Vec<S> {
        let mut obj = vec![Obj::Unused; self.updates as usize + 1];
        obj[0] = Obj::Live;
        vec![S {
            mode: vec![Mode::Kernel; self.cpus],
            eqs: vec![false; self.cpus],
            ptr: 0,
            obj,
            mon: Mon::new(),
            now: 0,
            updates: 0,
        }]
    }

    fn actions(&self, s: &S, actions: &mut Vec<Action>) {
        for c in 0..self.cpus {
            match s.mode[c] {
                Mode::Kernel => {
                    actions.push(Action::Read(c));
                    if s.updates < self.updates {
                        actions.push(Action::Update(c));
                    }
                    actions.push(Action::ToUser(c));
                    actions.push(Action::Switch(c));
                    actions.push(Action::Halt(c));
                }
                Mode::Read(_) => actions.push(Action::Unlock(c)),
                Mode::User => actions.push(Action::Enter(c)),
                Mode::Idle => {
                    actions.push(Action::Wake(c));
                    actions.push(Action::Irq(c));
                }
                Mode::IrqRead(_) => actions.push(Action::IrqDone(c)),
            }
        }
        // Assumption R: a read-side section ends within the tick it started in (it runs with preemption off).
        let reading = s
            .mode
            .iter()
            .any(|m| matches!(m, Mode::Read(_) | Mode::IrqRead(_)));
        if s.now < self.max_time && !reading {
            actions.push(Action::Tick);
        }
    }

    fn next_state(&self, last: &S, action: Action) -> Option<S> {
        let mut s = last.clone();
        match action {
            Action::Read(c) => s.mode[c] = Mode::Read(s.ptr),
            Action::Unlock(c) => s.mode[c] = Mode::Kernel,
            Action::Update(_) => {
                let old = s.ptr;
                s.updates += 1;
                s.ptr = s.updates;
                s.obj[s.ptr as usize] = Obj::Live;
                s.obj[old as usize] = Obj::Retired(s.now);
                let counted = self.counted(&s);
                s.mon.retire(old, counted);
            }
            Action::ToUser(c) => {
                self.qs(&mut s, c);
                s.mode[c] = Mode::User;
            }
            Action::Enter(c) => s.mode[c] = Mode::Kernel,
            Action::Switch(c) => self.qs(&mut s, c),
            Action::Halt(c) => {
                self.qs(&mut s, c);
                s.mode[c] = Mode::Idle;
                if self.extended() {
                    s.eqs[c] = true;
                }
            }
            Action::Wake(c) => {
                s.mode[c] = Mode::Kernel;
                s.eqs[c] = false;
            }
            Action::Irq(c) => {
                if self.idle == IdleQs::Extended {
                    s.eqs[c] = false;
                }
                s.mode[c] = Mode::IrqRead(s.ptr);
            }
            Action::IrqDone(c) => {
                // Back in the idle loop, `halt_cpu` runs again and reports, then idles (and marks itself idle again).
                // With the extended state this report is what ends a grace period that started while the handler read:
                // without it the last CPU to idle can leave one waiting for good (the model found that, 2026-11-08).
                self.qs(&mut s, c);
                s.mode[c] = Mode::Idle;
                if self.extended() {
                    s.eqs[c] = true;
                }
            }
            Action::Tick => {
                s.now += 1;
                for c in 0..self.cpus {
                    let reached = match s.mode[c] {
                        Mode::User => true,
                        Mode::Kernel => self.kernel_qs_per_tick,
                        Mode::Idle => self.tick == Tick::Periodic && !self.extended(),
                        Mode::Read(_) | Mode::IrqRead(_) => false,
                    };
                    if reached {
                        self.qs(&mut s, c);
                    }
                }
            }
        }
        Some(s)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            // Layer (c): no reader holds a freed object.
            Property::always("no use after free", |_, s: &S| {
                s.mode.iter().all(|m| match m {
                    Mode::Read(o) | Mode::IrqRead(o) => s.obj[*o as usize] != Obj::Freed,
                    _ => true,
                })
            }),
            Property::always("the published object is never freed", |_, s: &S| {
                s.obj[s.ptr as usize] == Obj::Live
            }),
            // Layer (d): freed within the bound.
            Property::always("freed within the bound", |m: &Rcu, s: &S| {
                s.obj.iter().all(|o| match o {
                    Obj::Retired(t) => s.now <= t + m.bound,
                    _ => true,
                })
            }),
            // Non-vacuity.
            Property::sometimes("an object is freed", |_, s: &S| s.obj.contains(&Obj::Freed)),
            Property::sometimes("an object is freed while a CPU idles", |_, s: &S| {
                s.obj.contains(&Obj::Freed) && s.mode.contains(&Mode::Idle)
            }),
            Property::sometimes("an idle CPU reads in an interrupt", |_, s: &S| {
                s.mode.iter().any(|m| matches!(m, Mode::IrqRead(_)))
            }),
        ]
    }

    fn within_boundary(&self, s: &S) -> bool {
        s.now <= self.max_time
    }
}
