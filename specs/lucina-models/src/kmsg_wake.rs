//! The wake of `/dev/kmsg` readers (kernel patch 1260): spec `docs/spec/kmsg-wake.md`, layers (c) and (d).
//!
//! What is modelled: writers that log one line each, every one on a CPU and in a context of its own (a task, or an
//! interrupt handler), from `ostd/src/log/kmsg.rs`; the per-CPU softirq bit and what makes a CPU run its softirqs
//! (`aster_softirq`), with the softirq `wake_readers` of `kernel/core/src/device/mem/kmsg.rs`; one reader in
//! `KmsgFile::read_at`, which registers a poller on the readers' `Pollee`, looks at the ring, and sleeps. Every step is
//! one atomic action: the publish under the ring's lock, each `swap` of `WAKE_PENDING`, the raise with interrupts off,
//! the reader's look under the ring's lock. The steps of different actors interleave in every order, including steps of
//! two writers on the same CPU (an interrupt writer between a task writer's steps): that is more than the hardware does,
//! which is safe for the properties checked.
//!
//! A CPU runs its softirqs only when something makes it ([`S::deliver`]): the exit of the interrupt a writer logged
//! from, the SGI a task writer sends to its own CPU, or the softirq loop that is already running there (it reads the
//! pending bits again after each run). No periodic tick is assumed: an idle CPU's tick may be stopped
//! (`lucina.cpuidle=cpu`), so a bit that nothing delivers stays pending for as long as the CPU sleeps.
//!
//! Lost wake-up: the reader sleeps, a record it has not read is in the ring, and nothing that could wake it is left to
//! happen. That state is a deadlock of the system, so it is an `always` property over a state, not an `eventually`.

use stateright::{Model, Property};

/// Where a writer logs from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Ctx {
    /// A task (any preemption state): the raise sends an SGI to its own CPU.
    Task,
    /// An interrupt handler: the softirqs run on the way out of the interrupt.
    Irq,
}

/// The design, or a witness with one rule broken.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Variant {
    /// Shipped (1260).
    Shipped,
    /// Witness: the reader sleeps right after registering, without looking at the ring again (it looked only before
    /// registering, in the fast path): the check-before-register bug.
    CheckBeforeRegister,
    /// Witness: the softirq notifies while the flag is on and clears it afterwards.
    NotifyBeforeClear,
    /// Witness: the writer turns the flag on (and raises) before its record is in the ring.
    FlagBeforePublish,
    /// Witness: a task writer only marks the softirq bit, without the SGI to its own CPU.
    NoSgi,
    /// Witness: the softirq loop ends after its callbacks without reading the pending bits again. A raise that lands on
    /// a CPU while its softirq run is in progress (an interrupt nested in the run: the interrupt's exit runs nothing,
    /// because a run is in progress) sets a bit that nothing processes before the CPU goes idle.
    NoRecheckAfterRun,
}

#[derive(Clone, Debug)]
pub struct KmsgWake {
    /// One line each: (CPU, context).
    pub writers: Vec<(usize, Ctx)>,
    pub cpus: usize,
    pub variant: Variant,
}

impl KmsgWake {
    pub fn shipped(writers: Vec<(usize, Ctx)>) -> Self {
        let cpus = writers.iter().map(|w| w.0 + 1).max().unwrap_or(1);
        KmsgWake {
            writers,
            cpus,
            variant: Variant::Shipped,
        }
    }

    pub fn with(self, variant: Variant) -> Self {
        KmsgWake { variant, ..self }
    }

    fn total(&self) -> u8 {
        self.writers.len() as u8
    }
}

/// A writer's next step.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Wpc {
    Start,
    /// The record is in the ring; next, the flag.
    Published,
    /// The writer turned the flag on; next, the raise.
    MustRaise,
    /// Only with [`Variant::FlagBeforePublish`]: flag and raise done, the publish is next.
    MustPublish,
    Done,
}

/// The softirq `wake_readers` on one CPU.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Cb {
    Idle,
    /// The softirq loop cleared the bit and runs the callback; next, `take_wake`.
    Running,
    /// `take_wake` returned this; next, the notify (only when true).
    Took(bool),
    /// Only with [`Variant::NotifyBeforeClear`]: notified (or not); next, clearing the flag.
    Notified,
}

/// The reader's next step in `read_at`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Rpc {
    /// The fast path: a look without a poller.
    Try,
    /// The look found nothing: register the poller.
    Register,
    /// Registered: look again.
    Look,
    /// In `poller.wait()`.
    Sleep,
    /// Every record read.
    Done,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct S {
    /// Records in the ring.
    pub seq: u8,
    /// `WAKE_PENDING`.
    pub flag: bool,
    /// The kmsg softirq's pending bit, per CPU.
    pub bit: Vec<bool>,
    /// Whether this CPU will run its softirqs (an interrupt exit, an SGI, or its softirq loop is running).
    pub deliver: Vec<bool>,
    pub cb: Vec<Cb>,
    pub w: Vec<Wpc>,
    pub r: Rpc,
    /// The reader's cursor: records read.
    pub cursor: u8,
    /// The poller is registered on the readers' `Pollee`.
    pub registered: bool,
    /// The poller's waker was woken and the wait has not consumed it.
    pub woken: bool,
    /// For the `sometimes` properties.
    pub coalesced: bool,
    pub woke_for_record: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    Publish(usize),
    Flag(usize),
    Raise(usize),
    /// The CPU's softirq loop takes the bit and runs `wake_readers`.
    Softirq(usize),
    Take(usize),
    Notify(usize),
    /// [`Variant::NotifyBeforeClear`]: the late clear.
    Clear(usize),
    Reader,
}

impl KmsgWake {
    fn quiescent(&self, s: &S) -> bool {
        s.w.iter().all(|w| *w == Wpc::Done)
            && s.cb.iter().all(|c| *c == Cb::Idle)
            && (0..self.cpus).all(|c| !(s.bit[c] && s.deliver[c]))
    }

    fn raise(&self, s: &mut S, i: usize) {
        let (cpu, ctx) = self.writers[i];
        s.bit[cpu] = true;
        if ctx == Ctx::Irq || self.variant != Variant::NoSgi {
            s.deliver[cpu] = true;
        }
    }
}

impl Model for KmsgWake {
    type State = S;
    type Action = Action;

    fn init_states(&self) -> Vec<S> {
        vec![S {
            seq: 0,
            flag: false,
            bit: vec![false; self.cpus],
            deliver: vec![false; self.cpus],
            cb: vec![Cb::Idle; self.cpus],
            w: vec![Wpc::Start; self.writers.len()],
            r: Rpc::Try,
            cursor: 0,
            registered: false,
            woken: false,
            coalesced: false,
            woke_for_record: false,
        }]
    }

    fn actions(&self, s: &S, actions: &mut Vec<Action>) {
        for (i, w) in s.w.iter().enumerate() {
            match (w, self.variant) {
                (Wpc::Start, Variant::FlagBeforePublish) => actions.push(Action::Flag(i)),
                (Wpc::Start, _) => actions.push(Action::Publish(i)),
                (Wpc::Published, Variant::FlagBeforePublish) => {}
                (Wpc::Published, _) => actions.push(Action::Flag(i)),
                (Wpc::MustRaise, _) => actions.push(Action::Raise(i)),
                (Wpc::MustPublish, _) => actions.push(Action::Publish(i)),
                (Wpc::Done, _) => {}
            }
        }
        for c in 0..self.cpus {
            match s.cb[c] {
                Cb::Idle if s.bit[c] && s.deliver[c] => actions.push(Action::Softirq(c)),
                Cb::Idle => {}
                Cb::Running => actions.push(Action::Take(c)),
                Cb::Took(_) => actions.push(Action::Notify(c)),
                Cb::Notified => actions.push(Action::Clear(c)),
            }
        }
        match s.r {
            Rpc::Done => {}
            Rpc::Sleep if !s.woken => {}
            _ => actions.push(Action::Reader),
        }
    }

    fn next_state(&self, last: &S, action: Action) -> Option<S> {
        let mut s = last.clone();
        match action {
            Action::Publish(i) => {
                s.seq += 1;
                s.w[i] = if s.w[i] == Wpc::MustPublish {
                    Wpc::Done
                } else {
                    Wpc::Published
                };
            }
            Action::Flag(i) => {
                let was = s.flag;
                s.flag = true;
                s.coalesced |= was;
                s.w[i] = if !was {
                    Wpc::MustRaise
                } else if self.variant == Variant::FlagBeforePublish {
                    Wpc::MustPublish
                } else {
                    Wpc::Done
                };
            }
            Action::Raise(i) => {
                self.raise(&mut s, i);
                s.w[i] = if self.variant == Variant::FlagBeforePublish {
                    Wpc::MustPublish
                } else {
                    Wpc::Done
                };
            }
            Action::Softirq(c) => {
                s.bit[c] = false;
                s.cb[c] = Cb::Running;
            }
            Action::Take(c) => {
                if self.variant == Variant::NotifyBeforeClear {
                    if s.flag && s.registered {
                        s.woken = true;
                    }
                    s.cb[c] = Cb::Notified;
                } else {
                    s.cb[c] = Cb::Took(s.flag);
                    s.flag = false;
                }
            }
            Action::Notify(c) => {
                if s.cb[c] == Cb::Took(true) && s.registered {
                    s.woken = true;
                }
                s.cb[c] = Cb::Idle;
                // The loop reads the pending bits again: it goes on while one is set.
                s.deliver[c] = s.bit[c];
                if self.variant == Variant::NoRecheckAfterRun {
                    // The loop ends here: a bit set during the run waits for the CPU's next raise.
                    s.deliver[c] = false;
                }
            }
            Action::Clear(c) => {
                s.flag = false;
                s.cb[c] = Cb::Idle;
                s.deliver[c] = s.bit[c];
            }
            Action::Reader => match s.r {
                // A look that finds records reads one; `read_at` returns, its poller goes, and the next `read` starts
                // with the fast path.
                Rpc::Try | Rpc::Look if s.seq > s.cursor => {
                    s.cursor += 1;
                    s.registered = false;
                    s.woken = false;
                    s.r = if s.cursor == self.total() {
                        Rpc::Done
                    } else {
                        Rpc::Try
                    };
                }
                Rpc::Try => s.r = Rpc::Register,
                Rpc::Register => {
                    s.registered = true;
                    s.r = if self.variant == Variant::CheckBeforeRegister {
                        Rpc::Sleep
                    } else {
                        Rpc::Look
                    };
                }
                Rpc::Look => s.r = Rpc::Sleep,
                Rpc::Sleep => {
                    // Woken: the wait returns and consumes the wake.
                    s.woken = false;
                    s.woke_for_record |= s.seq > s.cursor;
                    s.r = Rpc::Look;
                }
                Rpc::Done => return None,
            },
        }
        Some(s)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::always("no lost wake-up", |m: &KmsgWake, s: &S| {
                !(s.r == Rpc::Sleep && !s.woken && s.seq > s.cursor && m.quiescent(s))
            }),
            Property::always("the reader reads only what is in the ring", |_, s: &S| {
                s.cursor <= s.seq
            }),
            Property::sometimes("the reader sleeps and is woken for a record", |_, s: &S| {
                s.woke_for_record
            }),
            Property::sometimes("a raise is coalesced", |_, s: &S| s.coalesced),
            Property::sometimes("the reader reads every record", |_, s: &S| s.r == Rpc::Done),
            Property::sometimes(
                "a raise lands during its CPU's softirq run",
                |m: &KmsgWake, s: &S| (0..m.cpus).any(|c| s.bit[c] && s.cb[c] != Cb::Idle),
            ),
        ]
    }
}
