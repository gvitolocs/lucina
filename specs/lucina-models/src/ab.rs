//! Juno's A/B generations: install, trial, settle and rollback under power cuts. Spec `docs/spec/juno-ab-boot.md`.
//!
//! The loader's choices are the shipped functions: `lucina_boot::trial::{newest, decide, settle_step, settle_start}` (Verus
//! proves each on its own; this model checks what they do together, across boots, installs and cuts). The install's writes
//! follow `lucina-install-disk` (services/lucina-remote/src/bin/lucina-install-disk.rs) step by step; `lucina-trial` is the
//! good mark, the settle count and the rewrite of BOOT.CFG after a rollback.
//!
//! What a power cut leaves: every completed write; the root image being written is torn; a one-block write (BOOT.CFG, a
//! record copy) is whole or not done (assumption D1, the witness `torn_commit` drops it).

use lucina_boot::trial::{self, Copy as RecCopy, Record, Settle, Start, State as RecState};
use stateright::{Model, Property};

pub const MAX_GEN: usize = 4;

/// Layer (d) for a generation that dies after its good mark: at most this many more starts. With the proposed loader count,
/// at most `SETTLE_BAD` starts in a row die before 60 s, and the second boot that lasts rolls back through `settle_step`.
pub const RUNS_AFTER_MARK_BOUND: u32 = (trial::SETTLE_BAD + 1) * trial::SETTLE_BAD;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Slot {
    Empty,
    Whole(u8),
    Torn,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Cfg {
    Whole(u8),
    Torn,
}

/// A record copy, as the loader decodes it. `settle` is the record's settle window (AB-3; 0 when `settle_in_loader` is off).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rec {
    pub seq: u8,
    pub generation: u8,
    pub state: u8, // 0 trial, 1 good, 2 failed
    pub tries: u8,
    pub settle: u8,
}

impl Rec {
    fn to_trial(self) -> Record {
        Record {
            seq: self.seq as u32,
            generation: self.generation as u32,
            state: match self.state {
                0 => RecState::Trial,
                1 => RecState::Good,
                _ => RecState::Failed,
            },
            tries: self.tries as u32,
            settle: self.settle as u32,
        }
    }

    fn from_trial(r: Record) -> Rec {
        Rec {
            seq: r.seq as u8,
            generation: r.generation as u8,
            state: match r.state {
                RecState::Trial => 0,
                RecState::Good => 1,
                RecState::Failed => 2,
            },
            tries: r.tries as u8,
            settle: r.settle as u8,
        }
    }
}

/// What a new generation does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Behaviour {
    Good,
    /// Never lasts to the good mark (a crash, a hang, an early stall).
    CrashEarly,
    /// Marked good once, then dies at every later boot, at any moment: before userspace (gen 15 after a cold power cycle,
    /// 2026-11-02), or later (gen 32 vanishing every 100 s, 2026-11-05).
    CrashLate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Run {
    Off,
    Up {
        generation: u8,
        via_prev: bool,
        trial_done: bool,
        lasted: bool,
    },
    /// The kernel started but the root it was told to mount is not that generation's: the rescue initramfs (SSH).
    Rescue,
    /// The loader found nothing it could start: the recovery stick is the way back.
    Stuck,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Step {
    /// `drop_prev` only: PREV.CFG names the generation on the target slot; it is removed.
    DropPrev,
    /// The root image: block 0 first, so the slot is not that generation's any more.
    ImageStart,
    ImageDone,
    /// remove_unnamed(keep): kernels of generations neither configuration keeps.
    RemoveUnnamed,
    Kernel,
    PrevRemove,
    PrevCreate,
    Trial,
    Commit,
    Cleanup,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Install {
    pub generation: u8,
    pub slot: u8,
    pub pc: u8,
    pub drop_prev: bool,
    pub old: u8,
}

#[derive(Clone, Debug)]
pub struct Ab {
    pub max_gen: u8,
    pub max_boots: u8,
    pub behaviours: Vec<Behaviour>,
    /// Shipped `true` since 2026-11-08; `false` is finding AB-1: the generation PREV.CFG names is removed after its root slot
    /// has been overwritten.
    pub drop_prev_first: bool,
    /// Shipped `true` since 2026-11-08; `false` is finding AB-2: the target slot is the one BOOT.CFG does not boot, whatever
    /// runs.
    pub install_checks_running: bool,
    /// Shipped `true` since 2026-11-08 (`trial::mark_good`, `decide` and `lasted`); `false` is finding AB-3: after the good mark
    /// only `lucina-trial` counts bad boots, so one that dies before userspace is never counted.
    pub settle_in_loader: bool,
    /// Witness: every loader write of a record fails (the loader then boots without counting).
    pub record_writes_fail: bool,
    /// Witness: a cut during the commit tears BOOT.CFG (drops assumption D1).
    pub torn_commit: bool,
}

impl Ab {
    /// The design as proposed: shipped plus the three fixes.
    pub fn proposed() -> Self {
        Ab {
            max_gen: 3,
            max_boots: 6,
            behaviours: vec![Behaviour::Good, Behaviour::CrashEarly],
            drop_prev_first: true,
            install_checks_running: true,
            settle_in_loader: true,
            record_writes_fail: false,
            torn_commit: false,
        }
    }

    pub fn shipped() -> Self {
        Ab {
            // AB-1 and AB-2 are fixed in lucina-install-disk (2026-11-08): PREV.CFG goes before the image, and the install
            // refuses unless BOOT.CFG's generation runs.
            drop_prev_first: true,
            install_checks_running: true,
            settle_in_loader: true,
            ..Ab::proposed()
        }
    }

    fn steps(&self) -> &'static [Step] {
        use Step::*;
        if self.drop_prev_first {
            &[
                DropPrev,
                ImageStart,
                ImageDone,
                RemoveUnnamed,
                Kernel,
                PrevRemove,
                PrevCreate,
                Trial,
                Commit,
                Cleanup,
            ]
        } else {
            &[
                ImageStart,
                ImageDone,
                DropPrev,
                RemoveUnnamed,
                Kernel,
                PrevRemove,
                PrevCreate,
                Trial,
                Commit,
                Cleanup,
            ]
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct S {
    pub boot: Cfg,
    pub prev: Option<Cfg>,
    /// Bit g: generation g's kernel and initramfs are whole on the ESP.
    pub kernels: u8,
    pub recs: [Option<Rec>; 2],
    pub slots: [Slot; 2],
    pub slot_of: [u8; MAX_GEN],
    pub behaviour: [Behaviour; MAX_GEN],
    /// lucina-trial's settle count on /data.
    pub settle: Option<(u8, u8, u8)>,
    /// The boot before this one ended badly; this one so far.
    pub last_bad: bool,
    pub this_bad: bool,
    pub run: Run,
    pub install: Option<Install>,
    pub boots: u8,
    pub gens: u8,
    pub starts_on_trial: [u8; MAX_GEN],
    pub marked: [bool; MAX_GEN],
    pub runs_after_mark: [u8; MAX_GEN],
    pub wrote_running: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    Boot,
    TrialService,
    MarkGood,
    /// The boot has lasted 60 s (re-arms the proposed loader count).
    Lasted,
    Crash,
    Shutdown,
    PowerCut,
    InstallStart(Behaviour),
    InstallStep,
}

fn has(kernels: u8, g: u8) -> bool {
    kernels & (1 << g) != 0
}

fn cfg_gen(c: Option<Cfg>) -> Option<u8> {
    match c {
        Some(Cfg::Whole(g)) => Some(g),
        _ => None,
    }
}

impl Ab {
    /// PREV.CFG as the loader counts it: it parses, names another generation, and its kernel is there.
    fn previous(&self, s: &S, current: u8) -> Option<u8> {
        cfg_gen(s.prev).filter(|&p| p != current && has(s.kernels, p))
    }

    fn write_rec(s: &mut S, to: RecCopy, r: Rec) {
        s.recs[match to {
            RecCopy::A => 0,
            RecCopy::B => 1,
        }] = Some(r);
    }

    fn newest(s: &S) -> (Option<Rec>, RecCopy) {
        let n = trial::newest(s.recs[0].map(Rec::to_trial), s.recs[1].map(Rec::to_trial));
        (n.record.map(Rec::from_trial), n.write_to)
    }

    fn boot(&self, s: &mut S) {
        s.boots += 1;
        s.last_bad = s.this_bad;
        s.this_bad = true;
        let Cfg::Whole(g) = s.boot else {
            s.run = Run::Stuck;
            return;
        };
        let previous = self.previous(s, g);
        let (record, write_to) = Ab::newest(s);
        let mut start = Start::Current;
        if let Some(r) = record.filter(|r| r.generation == g) {
            // The shipped `trial::decide`: on trial, and (AB-3) a good generation in its settle window.
            let d = trial::decide(Some(r.to_trial()), g as u32, previous.is_some());
            if let Some(w) = d.write
                && !self.record_writes_fail
            {
                Ab::write_rec(s, write_to, Rec::from_trial(w));
            }
            start = d.start;
            if d.start == Start::Current && r.state == 0 {
                s.starts_on_trial[g as usize] += 1;
            }
        }
        let (x, via_prev) = match (start, previous) {
            (Start::Previous, Some(p)) => (p, true),
            _ => (g, false),
        };
        if !has(s.kernels, x) {
            s.run = Run::Stuck;
            return;
        }
        if s.slots[s.slot_of[x as usize] as usize] != Slot::Whole(x) {
            s.run = Run::Rescue;
            return;
        }
        if s.marked[x as usize] {
            s.runs_after_mark[x as usize] += 1;
        }
        s.run = Run::Up {
            generation: x,
            via_prev,
            trial_done: false,
            lasted: false,
        };
    }

    fn install_step(&self, s: &mut S) {
        let Some(mut inst) = s.install else { return };
        let step = self.steps()[inst.pc as usize];
        match step {
            Step::DropPrev => {
                if inst.drop_prev {
                    s.prev = None;
                }
            }
            Step::ImageStart => {
                if let Run::Up { generation, .. } = s.run
                    && s.slot_of[generation as usize] == inst.slot
                {
                    s.wrote_running = true;
                }
                s.slots[inst.slot as usize] = Slot::Torn;
            }
            Step::ImageDone => s.slots[inst.slot as usize] = Slot::Whole(inst.generation),
            Step::RemoveUnnamed => {
                let mut keep = 1 << inst.old;
                if let Some(p) = cfg_gen(s.prev) {
                    keep |= 1 << p;
                }
                s.kernels &= keep;
            }
            Step::Kernel => s.kernels |= 1 << inst.generation,
            Step::PrevRemove => s.prev = None,
            Step::PrevCreate => s.prev = Some(Cfg::Whole(inst.old)),
            Step::Trial => {
                let (record, write_to) = Ab::newest(s);
                let seq = record.map_or(1, |r| r.seq + 1);
                Ab::write_rec(
                    s,
                    write_to,
                    Rec {
                        seq,
                        generation: inst.generation,
                        state: 0,
                        tries: trial::TRIES as u8,
                        settle: 0,
                    },
                );
            }
            Step::Commit => s.boot = Cfg::Whole(inst.generation),
            Step::Cleanup => s.kernels &= (1 << inst.generation) | (1 << inst.old),
        }
        inst.pc += 1;
        s.install = if (inst.pc as usize) < self.steps().len() {
            Some(inst)
        } else {
            None
        };
    }
}

impl Model for Ab {
    type State = S;
    type Action = Action;

    fn init_states(&self) -> Vec<S> {
        let mut slot_of = [9u8; MAX_GEN];
        slot_of[1] = 0;
        vec![S {
            boot: Cfg::Whole(1),
            prev: None,
            kernels: 1 << 1,
            recs: [None, None],
            slots: [Slot::Whole(1), Slot::Empty],
            slot_of,
            behaviour: [Behaviour::Good; MAX_GEN],
            settle: None,
            last_bad: false,
            this_bad: false,
            run: Run::Off,
            install: None,
            boots: 0,
            gens: 1,
            starts_on_trial: [0; MAX_GEN],
            marked: [false; MAX_GEN],
            runs_after_mark: [0; MAX_GEN],
            wrote_running: false,
        }]
    }

    fn actions(&self, s: &S, actions: &mut Vec<Action>) {
        match s.run {
            Run::Off => {
                if s.boots < self.max_boots {
                    actions.push(Action::Boot);
                }
            }
            Run::Stuck => {}
            Run::Rescue => actions.push(Action::PowerCut),
            Run::Up {
                generation,
                via_prev,
                trial_done,
                lasted,
            } => {
                let b = s.behaviour[generation as usize];
                let marked = s.marked[generation as usize];
                actions.push(Action::PowerCut);
                if (b == Behaviour::CrashEarly && !marked) || (b == Behaviour::CrashLate && marked)
                {
                    actions.push(Action::Crash);
                }
                if !trial_done {
                    actions.push(Action::TrialService);
                    return;
                }
                let dies = b == Behaviour::CrashEarly || (b == Behaviour::CrashLate && marked);
                if !lasted && b != Behaviour::CrashEarly {
                    actions.push(Action::Lasted);
                }
                if lasted && !marked && !via_prev && b != Behaviour::CrashEarly {
                    let (record, _) = Ab::newest(s);
                    if record.is_some_and(|r| r.generation == generation && r.state == 0) {
                        actions.push(Action::MarkGood);
                    }
                }
                if !dies && s.install.is_none() {
                    actions.push(Action::Shutdown);
                }
                if s.install.is_some() {
                    actions.push(Action::InstallStep);
                } else if s.gens < self.max_gen && !dies && lasted {
                    let current = cfg_gen(Some(s.boot));
                    // Shipped installs from any running system; the proposed check refuses unless BOOT.CFG's generation runs.
                    if !self.install_checks_running || current == Some(generation) {
                        for &bh in &self.behaviours {
                            actions.push(Action::InstallStart(bh));
                        }
                    }
                }
            }
        }
    }

    fn next_state(&self, last: &S, action: Action) -> Option<S> {
        let mut s = last.clone();
        match action {
            Action::Boot => self.boot(&mut s),
            Action::TrialService => {
                let Run::Up {
                    generation,
                    via_prev,
                    lasted,
                    ..
                } = s.run
                else {
                    return None;
                };
                if via_prev {
                    // `lucina.trial=rolledback:<g>`: BOOT.CFG is rewritten from PREV.CFG.
                    s.boot = Cfg::Whole(generation);
                } else {
                    let prev = s.settle.map(|(g, b, bad)| Settle {
                        generation: g as u32,
                        boots: b as u32,
                        bad: bad as u32,
                    });
                    let have_previous = self.previous(&s, generation).is_some();
                    let step =
                        trial::settle_step(prev, generation as u32, s.last_bad, have_previous);
                    s.settle = step
                        .next
                        .map(|n| (n.generation as u8, n.boots as u8, n.bad as u8));
                    if step.roll_back
                        && let Some(p) = s.prev
                    {
                        s.boot = p;
                    }
                }
                s.run = Run::Up {
                    generation,
                    via_prev,
                    trial_done: true,
                    lasted,
                };
            }
            Action::Lasted => {
                let Run::Up {
                    generation,
                    via_prev,
                    trial_done,
                    ..
                } = s.run
                else {
                    return None;
                };
                s.run = Run::Up {
                    generation,
                    via_prev,
                    trial_done,
                    lasted: true,
                };
                if self.settle_in_loader {
                    // lucina-trial's `settle_lasted`: the shipped `trial::lasted`.
                    let (record, write_to) = Ab::newest(&s);
                    if let Some(w) = record.and_then(|r| trial::lasted(r.to_trial(), generation as u32)) {
                        Ab::write_rec(&mut s, write_to, Rec::from_trial(w));
                    }
                }
            }
            Action::MarkGood => {
                let Run::Up { generation, .. } = s.run else {
                    return None;
                };
                let (record, write_to) = Ab::newest(&s);
                let r = record?;
                let good = if self.settle_in_loader {
                    // lucina-trial's mark: the shipped `trial::mark_good`.
                    Rec::from_trial(trial::mark_good(r.to_trial()))
                } else {
                    // Before AB-3: good, no window.
                    Rec {
                        seq: r.seq + 1,
                        state: 1,
                        tries: trial::SETTLE_BAD as u8,
                        settle: 0,
                        ..r
                    }
                };
                Ab::write_rec(&mut s, write_to, good);
                s.marked[generation as usize] = true;
                let st = trial::settle_start(generation as u32);
                s.settle = Some((st.generation as u8, st.boots as u8, st.bad as u8));
            }
            Action::Crash => s.run = Run::Off,
            Action::Shutdown => {
                s.this_bad = false;
                s.run = Run::Off;
            }
            Action::PowerCut => {
                if let Some(inst) = s.install
                    && self.torn_commit
                    && self.steps()[inst.pc as usize] == Step::Commit
                {
                    s.boot = Cfg::Torn;
                }
                s.install = None;
                s.run = Run::Off;
            }
            Action::InstallStart(b) => {
                let Run::Up { .. } = s.run else { return None };
                let current = cfg_gen(Some(s.boot))?;
                // free_slot(slot_of(BOOT.CFG)), as lucina-install-disk does.
                let slot = 1 - s.slot_of[current as usize];
                let g = s.gens + 1;
                let drop_prev = cfg_gen(s.prev).is_some_and(|p| s.slot_of[p as usize] == slot);
                s.gens = g;
                s.slot_of[g as usize] = slot;
                s.behaviour[g as usize] = b;
                s.install = Some(Install {
                    generation: g,
                    slot,
                    pc: 0,
                    drop_prev,
                    old: current,
                });
            }
            Action::InstallStep => self.install_step(&mut s),
        }
        Some(s)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        fn whole(s: &S, g: u8) -> bool {
            has(s.kernels, g) && s.slots[s.slot_of[g as usize] as usize] == Slot::Whole(g)
        }
        vec![
            Property::always("BOOT.CFG names a whole generation", |_, s: &S| {
                match s.boot {
                    Cfg::Whole(g) => whole(s, g),
                    Cfg::Torn => false,
                }
            }),
            Property::always("PREV.CFG names a whole generation", |_, s: &S| {
                match s.prev {
                    Some(Cfg::Whole(g)) => whole(s, g),
                    Some(Cfg::Torn) => false,
                    None => true,
                }
            }),
            Property::always("the loader always starts something", |_, s: &S| {
                s.run != Run::Stuck
            }),
            Property::always(
                "the loader never starts a generation whose root is gone",
                |_, s: &S| s.run != Run::Rescue,
            ),
            Property::always("the running root is never written", |_, s: &S| {
                !s.wrote_running
            }),
            Property::always(
                "a generation that never lasts starts at most TRIES times",
                |_, s: &S| {
                    (0..MAX_GEN).all(|g| {
                        s.behaviour[g] != Behaviour::CrashEarly
                            || s.starts_on_trial[g] as u32 <= trial::TRIES
                    })
                },
            ),
            Property::always(
                "a generation that dies after its good mark is sent back in bounded boots",
                |_, s: &S| {
                    (0..MAX_GEN).all(|g| {
                        s.behaviour[g] != Behaviour::CrashLate
                            || s.runs_after_mark[g] as u32 <= RUNS_AFTER_MARK_BOUND
                    })
                },
            ),
            Property::sometimes("a new generation is marked good", |_, s: &S| {
                s.marked[2..].iter().any(|m| *m)
            }),
            Property::sometimes("the loader starts PREV.CFG", |_, s: &S| {
                matches!(s.run, Run::Up { via_prev: true, .. })
            }),
            Property::sometimes("a second install replaces PREV.CFG's slot", |_, s: &S| {
                s.gens >= 3
            }),
        ]
    }

    fn within_boundary(&self, s: &S) -> bool {
        s.boots <= self.max_boots
    }
}
