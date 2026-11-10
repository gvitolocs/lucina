// Verus proof of the SHIPPED code, not a model of it (project rule D-011): this file includes
// kernel/lucina-taskcpu/src/lib.rs by #[path], so Verus checks the functions as the kernel compiles them; what is
// proved of each is in its `ensures`. On top of that, the protocol model below — a state machine of the kernel's
// lock discipline whose steps call the shipped `decode`/`encode_on`/`after_failed_claim` — proves that once a task
// has exited it is never in a run queue nor current anywhere and its slot stays DEAD forever, over all
// interleavings of any number of wakers, and that the old behaviour (exit storing NONE) reaches a state where an
// exited task is enqueued.
#![feature(proc_macro_hygiene)]
#[path = "../../kernel/lucina-taskcpu/src/lib.rs"]
pub mod taskcpu;

use taskcpu::*;
use vstd::prelude::*;

verus! {

/// The round trip: a CPU id encodes to itself and decodes back to `On` of it.
fn decode_encode_on(c: u32) -> (r: Slot)
    requires
        c < DEAD,
    ensures
        r == Slot::On(c),
{
    decode(encode_on(c))
}

// ---------------------------------------------------------------------------
// The protocol model: one task, N CPUs, the kernel's lock discipline.
//
// Each step below is atomic, exactly as in the kernel: the waker's CAS is one step; the waker's
// lock + recheck + enqueue runs under the run-queue lock, so it is one step; park, pick and exit each
// run under the task's own CPU's run-queue lock, so each is one step. The wakers are a Vec indexed by
// waker id, so any number of them is in flight; `claim` is the CPU of the one held claim (at most one
// waker can hold it, because a claim needs the slot to be NONE).
// ---------------------------------------------------------------------------

/// A waker's program counter: Start -> Claimed(cpu) or Recheck(p) -> Done.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WakerPc {
    Start,
    Claimed(u32),
    Recheck(u32),
    Done,
}

/// The state: the task's raw slot value, which run queue holds it (`queued`), which CPU runs it as current
/// (`current`), whether it has exited, the in-flight wakers' program counters, and the held claim.
pub struct Model {
    pub slot: u32,
    pub queued: Option<u32>,
    pub current: Option<u32>,
    pub exited: bool,
    pub wakers: Vec<WakerPc>,
    pub claim: Option<u32>,
}

/// The inductive invariant. The last line is the goal: once exited, the task is in no run queue and is no CPU's
/// current (and by the second line its slot is DEAD).
pub open spec fn inv(m: Model) -> bool {
    let wakers = m.wakers@;
    &&& m.slot == NONE ==> m.queued.is_none() && m.current.is_none() && !m.exited
    &&& (m.slot == DEAD) == m.exited
    &&& m.slot != NONE && m.slot != DEAD ==> m.queued == Some(m.slot) || m.current == Some(m.slot)
            || m.claim == Some(m.slot)
    &&& !(m.queued.is_some() && m.current.is_some())
    &&& (forall|c: u32| m.queued == Some(c) ==> c < DEAD)
    &&& (forall|c: u32| m.current == Some(c) ==> c < DEAD)
    &&& (forall|c: u32| m.claim == Some(c) ==> c < DEAD && c == m.slot)
    &&& (forall|w: int, c: u32| 0 <= w < wakers.len() && wakers[w] == WakerPc::Claimed(c)
            ==> c < DEAD && c == m.slot && m.claim == Some(c))
    &&& (forall|w: int, p: u32| 0 <= w < wakers.len() && wakers[w] == WakerPc::Recheck(p)
            ==> p != NONE && p < DEAD)
    &&& (forall|w1: int, w2: int, c: u32| 0 <= w1 < wakers.len() && 0 <= w2 < wakers.len()
            && wakers[w1] == WakerPc::Claimed(c) && wakers[w2] == WakerPc::Claimed(c) ==> w1 == w2)
    &&& (m.queued.is_some() || m.current.is_some()) ==> m.claim.is_none()
    &&& m.exited ==> m.queued.is_none() && m.current.is_none()
}

/// One atomic step of the protocol.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
    Claim(usize, u32),
    Enqueue(usize),
    Park(u32),
    Pick(u32),
    Exit(u32),
}

/// Step 1 of a waker: the CAS NONE -> selected. On success the waker holds the claim; on failure the shipped
/// `after_failed_claim` decides: recheck under the previous CPU's run-queue lock, or refuse (the task exited).
fn waker_claim(m: &mut Model, w: usize, sel: u32)
    requires
        inv(*old(m)),
        w < old(m).wakers@.len(),
        old(m).wakers@[w as int] == WakerPc::Start,
        sel < DEAD,
    ensures
        inv(*final(m)),
        final(m).slot == old(m).slot || final(m).slot == sel,
        final(m).current == old(m).current,
        final(m).queued == old(m).queued,
        final(m).exited == old(m).exited,
        final(m).wakers@.len() == old(m).wakers@.len(),
        old(m).slot != NONE && old(m).slot != DEAD ==> final(m).wakers@[w as int] == WakerPc::Recheck(old(m).slot),
        old(m).slot == DEAD ==> final(m).wakers@[w as int] == WakerPc::Done,
{
    if m.slot == NONE {
        let raw = encode_on(sel);
        m.slot = raw;
        m.claim = Some(sel);
        m.wakers[w] = WakerPc::Claimed(sel);
    } else {
        match after_failed_claim(m.slot) {
            Wake::Refuse => {
                m.wakers[w] = WakerPc::Done;
            }
            Wake::Recheck(p) => {
                m.wakers[w] = WakerPc::Recheck(p);
            }
        }
    }
}

/// Steps 2-4 of a waker, atomic because they run under the run-queue lock: a held claim enqueues; a recheck
/// re-reads the slot under the lock (the shipped `decode`) and enqueues only if it is still Free (a DEAD slot
/// is left alone).
fn waker_enqueue(m: &mut Model, w: usize)
    requires
        inv(*old(m)),
        w < old(m).wakers@.len(),
    ensures
        inv(*final(m)),
{
    match m.wakers[w] {
        WakerPc::Claimed(c) => {
            proof! { assert(m.wakers@[w as int] == WakerPc::Claimed(c)); }
            m.queued = Some(c);
            m.claim = None;
            m.wakers[w] = WakerPc::Done;
        }
        WakerPc::Recheck(p) => {
            if m.slot == NONE {
                let d = decode(m.slot);
                proof! {
                    assert(d == Slot::Free);
                    if m.claim.is_some() {
                        let c = m.claim.unwrap();
                        assert(c < DEAD && c == m.slot);
                        assert(m.slot == NONE);
                        assert(c != NONE);
                    }
                    assert(m.claim.is_none());
                }
                m.slot = p;
                m.queued = Some(p);
            }
            m.wakers[w] = WakerPc::Done;
        }
        _ => {}
    }
}

/// The task parks, under its own CPU's run-queue lock: current -> none, slot := NONE.
fn task_park(m: &mut Model, c: u32)
    requires
        inv(*old(m)),
        old(m).current == Some(c),
    ensures
        inv(*final(m)),
{
    m.current = None;
    m.slot = NONE;
}

/// A CPU picks the task out of its run queue: slot stays On(c), current on c.
fn task_pick(m: &mut Model, c: u32)
    requires
        inv(*old(m)),
        old(m).queued == Some(c),
    ensures
        inv(*final(m)),
{
    m.queued = None;
    m.current = Some(c);
}

/// The task exits, under its own CPU's run-queue lock: current -> none, slot := DEAD (Linux's TASK_DEAD).
fn task_exit(m: &mut Model, c: u32)
    requires
        inv(*old(m)),
        old(m).current == Some(c),
    ensures
        inv(*final(m)),
{
    m.current = None;
    m.slot = DEAD;
    m.exited = true;
}

/// One atomic step, dispatched; a step whose precondition does not hold changes nothing.
fn step(m: &mut Model, s: Step)
    requires
        inv(*old(m)),
    ensures
        inv(*final(m)),
{
    match s {
        Step::Claim(w, sel) => {
            if w < m.wakers.len() && sel < DEAD {
                match m.wakers[w] {
                    WakerPc::Start => {
                        waker_claim(m, w, sel);
                    }
                    _ => {}
                }
            }
        }
        Step::Enqueue(w) => {
            if w < m.wakers.len() {
                waker_enqueue(m, w);
            }
        }
        Step::Park(c) => {
            if m.current == Some(c) {
                task_park(m, c);
            }
        }
        Step::Pick(c) => {
            if m.queued == Some(c) {
                task_pick(m, c);
            }
        }
        Step::Exit(c) => {
            if m.current == Some(c) {
                task_exit(m, c);
            }
        }
    }
}

/// Any sequence of steps — all interleavings, any number of wakers — keeps the invariant.
fn run(m: &mut Model, steps: Vec<Step>, i: usize)
    requires
        inv(*old(m)),
        i <= steps@.len(),
    ensures
        inv(*final(m)),
    decreases steps@.len() - i as int,
{
    if i < steps.len() {
        step(m, steps[i]);
        run(m, steps, i + 1);
    }
}

/// The theorem: over all interleavings, once the task has exited it is never in a run queue nor current anywhere,
/// and its slot stays DEAD forever.
fn theorem_dead_never_enqueued(m: &mut Model, steps: Vec<Step>)
    requires
        inv(*old(m)),
    ensures
        inv(*final(m)),
        final(m).exited ==> final(m).slot == DEAD && final(m).queued.is_none() && final(m).current.is_none(),
{
    run(m, steps, 0);
}

// ---------------------------------------------------------------------------
// The old behaviour fails: with exit storing NONE, an exited task is enqueued.
// ---------------------------------------------------------------------------

/// The old exit (the bug): stores NONE instead of DEAD.
fn task_exit_old(m: &mut Model, c: u32)
    requires
        old(m).current == Some(c),
    ensures
        final(m).current.is_none(),
        final(m).slot == NONE,
        final(m).exited,
        final(m).queued == old(m).queued,
        final(m).wakers@ == old(m).wakers@,
{
    m.current = None;
    m.slot = NONE;
    m.exited = true;
}

/// The waker's recheck + enqueue without the invariant as a precondition (the trace leaves the invariant).
fn waker_enqueue_old(m: &mut Model, w: usize)
    requires
        w < old(m).wakers@.len(),
    ensures
        forall|p: u32| old(m).wakers@[w as int] == WakerPc::Recheck(p) && old(m).slot == NONE
            ==> final(m).slot == p && final(m).queued == Some(p),
        final(m).exited == old(m).exited,
{
    match m.wakers[w] {
        WakerPc::Recheck(p) => {
            if m.slot == NONE {
                let d = decode(m.slot);
                proof! { assert(d == Slot::Free); }
                m.slot = p;
                m.queued = Some(p);
            }
            m.wakers[w] = WakerPc::Done;
        }
        _ => {}
    }
}

/// A concrete trace of the old behaviour: the task runs on CPU 0; waker 0's CAS fails and sees On(0), so the
/// shipped `after_failed_claim` gives Recheck(0); the task exits and the old code stores NONE; waker 0's recheck
/// under run-queue 0's lock then finds Free, claims it and enqueues the dead task.
fn old_behaviour_enqueues_the_dead_task() {
    let raw0 = encode_on(0);
    let mut m = Model {
        slot: raw0,
        queued: None,
        current: Some(0),
        exited: false,
        wakers: vec![WakerPc::Start],
        claim: None,
    };
    waker_claim(&mut m, 0, 1);
    assert(m.wakers@[0] == WakerPc::Recheck(0));
    task_exit_old(&mut m, 0);
    waker_enqueue_old(&mut m, 0);
    assert(m.exited && m.queued == Some(0) && m.slot == 0);
}

fn main() {}

} // verus!
