// SPDX-License-Identifier: MPL-2.0

//! RCU grace-period bookkeeping for Juno's kernel, tested on the host and proved by Verus in this very file
//! (`verification/juno-rcu/rules.rs`, D-011). `docs/research/juno-rcu.md`.
//!
//! The original is Asterinas's `ostd/src/sync/rcu/monitor.rs` (MPL-2.0): items retired while a grace period runs wait for
//! the next one; a grace period ends when every CPU has passed a quiescent point since it started; when it ends, the items
//! waiting for it are handed back to be freed and the waiting ones start the next grace period. The behaviour is kept
//! exactly, with the callbacks as items of any type and the CPU set as a 64-bit mask. The kernel calls [`Monitor::quiescent`]
//! under the monitor's lock on task switch, on going idle and on returning to user mode, and frees what it returns
//! outside the lock.
//!
//! What Verus proves, for any sequence of calls (`ensures` of each function):
//! * Safety: every item [`Monitor::quiescent`] returns was retired no later than the start of the grace period that
//!   returns it, and every CPU in `all` passed a quiescent point after that start.
//! * No loss: everything retired is, in order, what was returned, then the running grace period's items, then the
//!   waiting ones; each item is returned once.
//! * Progress: once a grace period runs, each CPU in `all` passing adds to it, and when all have passed it ends and
//!   returns its items (`round_ends`); the waiting items then start the next one, so an item comes back within two.
//!
//! `all` is not stored: the kernel passes its fixed mask of CPUs every time. A grace period ends when every CPU in the
//! mask given at that moment has passed, which is what Asterinas's `is_full` checks too, so the safety holds for that mask
//! even if it changed.
//!
//! The ghost history (calls so far, retire times, each CPU's last quiescent point) exists only under Verus; the kernel
//! build has the five plain fields.
//!
//! The items are kept in a [`Queue`], not one growing `Vec` (patch 1059). In the kernel no allocation may be larger than
//! a page: a larger one needs physically contiguous memory, which a fragmented heap may not have, and an infallible one
//! that fails panics. Asterinas's `VecDeque` of callbacks doubled while a grace period could not end and reached 2 MiB
//! before 1050 fixed the grace periods; a burst of unmaps between two quiescent points can still queue many thousands.
//! A [`Queue`] holds full chunks of [`CHUNK`] items in a linked list and fills one more, so its largest allocation is
//! `CHUNK` items (2 KiB for the kernel's boxed callbacks), however many wait. Verus proves it keeps every item, in order.
#![no_std]
#![allow(unexpected_cfgs)]

extern crate alloc;

use alloc::boxed::Box;
use alloc::vec::Vec;

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

/// Items per chunk of a [`Queue`]: its largest allocation is this many items.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const CHUNK: usize = 128;

/// A full chunk of [`CHUNK`] items, and the chunks filled before it.
#[cfg_attr(verus_keep_ghost, verus_verify)]
struct Chunk<T> {
    items: Vec<T>,
    older: Option<Box<Chunk<T>>>,
}

/// A first-in first-out list of items in chunks: the full ones newest first, linked, and one being filled. Nothing in it
/// allocates more than [`CHUNK`] items at once. Taking from it gives the newest chunk first (see [`Queue::take_last`]).
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub struct Queue<T> {
    /// The newest full chunk, which links to the older ones.
    full: Option<Box<Chunk<T>>>,
    /// Fewer than [`CHUNK`] items, the newest.
    fill: Vec<T>,
}

#[cfg(verus_keep_ghost)]
verus! {

/// The items of the chunks from `c` down, oldest first.
closed spec fn flat<T>(c: Option<Box<Chunk<T>>>) -> Seq<T>
    decreases c,
{
    match c {
        None => Seq::empty(),
        Some(b) => flat(b.older) + b.items@,
    }
}

/// Every chunk from `c` down is full.
closed spec fn whole<T>(c: Option<Box<Chunk<T>>>) -> bool
    decreases c,
{
    match c {
        None => true,
        Some(b) => b.items@.len() == CHUNK && whole(b.older),
    }
}

impl<T> View for Queue<T> {
    type V = Seq<T>;

    /// The items, oldest first.
    closed spec fn view(&self) -> Seq<T> {
        flat(self.full) + self.fill@
    }
}

impl<T> Queue<T> {
    /// Full chunks are full and the one being filled is not.
    pub closed spec fn wf(&self) -> bool {
        whole(self.full) && self.fill@.len() < CHUNK
    }
}

} // verus!

#[cfg_attr(verus_keep_ghost, verus_verify)]
impl<T> Queue<T> {
    /// No items.
    #[cfg_attr(verus_keep_ghost, verus_spec(q =>
        ensures
            q.wf(),
            q@.len() == 0,
    ))]
    pub const fn new() -> Self {
        let q = Queue { full: None, fill: Vec::new() };
        #[cfg(verus_keep_ghost)]
        proof! { assert(q@ =~= Seq::<T>::empty()); }
        q
    }

    /// Whether it holds no items.
    #[cfg_attr(verus_keep_ghost, verus_spec(r =>
        requires
            self.wf(),
        ensures
            r == (self@.len() == 0),
    ))]
    pub fn is_empty(&self) -> bool {
        self.full.is_none() && self.fill.len() == 0
    }

    /// Adds `item` last. Allocates at most [`CHUNK`] items, or one link.
    #[cfg_attr(verus_keep_ghost, verus_spec(
        requires
            old(self).wf(),
        ensures
            final(self).wf(),
            final(self)@ == old(self)@.push(item),
    ))]
    pub fn push(&mut self, item: T) {
        self.fill.push(item);
        if self.fill.len() == CHUNK {
            let mut items = Vec::new();
            core::mem::swap(&mut items, &mut self.fill);
            let mut older = None;
            core::mem::swap(&mut older, &mut self.full);
            self.full = Some(Box::new(Chunk { items, older }));
        }
        #[cfg(verus_keep_ghost)]
        proof! { assert(self@ =~= old(self)@.push(item)); }
    }

    /// Takes the newest items: the chunk being filled if it has any, else the newest full chunk, so that what is left
    /// followed by what it returns is what was there. Empty only when the queue was. Repeat until it returns an empty
    /// `Vec` to take everything, at most [`CHUNK`] items at a time.
    #[cfg_attr(verus_keep_ghost, verus_spec(r =>
        requires
            old(self).wf(),
        ensures
            final(self).wf(),
            old(self)@ == final(self)@ + r@,
            r@.len() <= CHUNK,
            (r@.len() == 0) == (old(self)@.len() == 0),
    ))]
    pub fn take_last(&mut self) -> Vec<T> {
        let mut out = Vec::new();
        if self.fill.len() > 0 {
            core::mem::swap(&mut out, &mut self.fill);
            #[cfg(verus_keep_ghost)]
            proof! { assert(old(self)@ =~= self@ + out@); }
            return out;
        }
        let mut top = None;
        core::mem::swap(&mut top, &mut self.full);
        match top {
            None => {
                #[cfg(verus_keep_ghost)]
                proof! { assert(old(self)@ =~= self@ + out@); }
                out
            }
            Some(boxed) => {
                let Chunk { items, older } = *boxed;
                self.full = older;
                #[cfg(verus_keep_ghost)]
                proof! { assert(old(self)@ =~= self@ + items@); }
                items
            }
        }
    }
}

/// Dropped chunk by chunk: the default drop would recurse once per chunk, deep enough to overflow a kernel stack.
#[cfg(not(verus_keep_ghost))]
impl<T> Drop for Queue<T> {
    fn drop(&mut self) {
        let mut top = self.full.take();
        while let Some(mut chunk) = top {
            top = chunk.older.take();
        }
    }
}

#[cfg(not(verus_keep_ghost))]
impl<T> Default for Queue<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// How much RCU work one quiescent state may do (patch 1112): at most `chunks` chunks of ready callbacks, and no new chunk
/// once `micros` have passed, the first always. Linux's `rcu_do_batch` bounds its batch the same two ways (`blimit` and a
/// time limit). The kernel loops: take a chunk ([`Monitor::take_ready`]), run it outside the lock, ask [`another_chunk`].
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budget {
    pub chunks: u32,
    pub micros: u64,
}

/// A CPU on its way between tasks or back to user mode: a few chunks, half a millisecond.
pub const BUSY: Budget = Budget { chunks: 4, micros: 500 };

/// A CPU about to go idle, which has nothing better to do: it drains first and most.
pub const IDLE: Budget = Budget { chunks: 64, micros: 4000 };

/// Linux's `rcu_divisor` (7): a batch takes at least 1/128 of the callbacks waiting.
pub const DIVISOR_SHIFT: u32 = 7;

/// Linux's `qhimark` (10000): with more callbacks than this waiting, the count limit goes and only time bounds a batch.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const QHIMARK: u64 = 10_000;

/// How long a busy CPU's batch may run when the callbacks flood: long enough to keep up with a process that unmaps a
/// gigabyte a second, far from a soft lockup (20 s).
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const FLOOD_MICROS: u64 = 2000;

/// The budget for a quiescent state with `pending` callbacks waiting (patch 1181), as Linux's `rcu_do_batch`: never less
/// than `base`; at least 1/128 of the backlog (in chunks); above [`QHIMARK`] no count limit and [`FLOOD_MICROS`] of time.
/// Frames freed by munmap and MADV_DONTNEED wait in these callbacks: with a fixed batch, a process that churns fast left a
/// gigabyte waiting and the laptop ran out of memory (2026-11-08 15:21).
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        // Never less than the base.
        r.chunks >= base.chunks,
        r.micros >= base.micros,
        // A flood lifts the count and gets the flood time, no more.
        pending > QHIMARK ==> r.chunks == u32::MAX,
        pending > QHIMARK ==> r.micros == (if base.micros > FLOOD_MICROS { base.micros } else { FLOOD_MICROS }),
        // Below it, the time stays the base's, and the count is the larger of the base's and 1/128 of the backlog.
        pending <= QHIMARK ==> r.micros == base.micros,
        pending <= QHIMARK ==> r.chunks as int == (if base.chunks as int >= (pending as int / 128 + 127) / 128 { base.chunks as int } else { (pending as int / 128 + 127) / 128 }),
))]
pub fn budget_for(base: Budget, pending: u64) -> Budget {
    if pending > QHIMARK {
        let micros = if base.micros > FLOOD_MICROS { base.micros } else { FLOOD_MICROS };
        return Budget { chunks: u32::MAX, micros };
    }
    // The backlog's share, in chunks of `CHUNK` (128), rounded up; below `QHIMARK` it is at most a chunk or so.
    let share = (pending / 128 + 127) / 128;
    let chunks = if (base.chunks as u64) >= share { base.chunks } else { share as u32 };
    Budget { chunks, micros: base.micros }
}

/// Whether a quiescent state takes another chunk, having taken `taken` and spent `elapsed_micros`.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        // Progress: the first chunk always goes, so ready items leave even with a budget of nothing.
        taken == 0 ==> r,
        // The count: never past `chunks` (one when `chunks` is 0).
        r ==> taken == 0 || taken < budget.chunks,
        // The time: no new chunk once the time is up.
        r ==> taken == 0 || elapsed_micros < budget.micros,
        // Within both, it goes on.
        taken < budget.chunks && elapsed_micros < budget.micros ==> r,
))]
pub fn another_chunk(budget: Budget, taken: u32, elapsed_micros: u64) -> bool {
    if taken == 0 {
        return true;
    }
    taken < budget.chunks && elapsed_micros < budget.micros
}

/// Items retired for freeing after a grace period, and which CPUs passed a quiescent point in the running one.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub struct Monitor<T> {
    /// The running grace period's items (Asterinas's `current_gp.callbacks`).
    current: Queue<T>,
    /// Items retired while it runs, for the next one (`next_callbacks`).
    next: Queue<T>,
    /// Items whose grace period ended, handed out a chunk at a time ([`Monitor::take_ready`]).
    ready: Queue<T>,
    /// Bit `c`: CPU `c` passed a quiescent point in the running grace period (`cpu_mask`).
    passed: u64,
    /// No grace period runs (`is_complete`).
    complete: bool,
    /// The fast-path flag (`is_monitoring`): a grace period runs, or items are ready.
    monitoring: bool,
    #[cfg(verus_keep_ghost)]
    history: Ghost<History<T>>,
}

#[cfg(verus_keep_ghost)]
verus! {

/// What has happened so far, for the proof only.
pub ghost struct History<T> {
    /// Calls so far: the clock. Each call ticks it first.
    pub now: nat,
    /// When the running grace period started.
    pub start: nat,
    /// Everything retired, in order.
    pub retired: Seq<T>,
    /// `at[i]`: when `retired[i]` was retired.
    pub at: Seq<nat>,
    /// `retired[..returned]` left the grace periods: handed out, or ready.
    pub returned: nat,
    /// The ready items are `retired[ready_start..ready_end]`; `retired[ready_end..returned]` was handed out, newest first.
    pub ready_start: nat,
    pub ready_end: nat,
    /// When the grace period that made them ready started, and the mask of CPUs that all passed since.
    pub ready_gp: nat,
    pub ready_all: u64,
    /// `last_qs[c]`: when CPU `c` last passed a quiescent point (0: never), 64 entries.
    pub last_qs: Seq<nat>,
}

/// CPU `c` is in the mask `m`.
pub open spec fn has(m: u64, c: u32) -> bool {
    c < 64 && (m >> (c as u64)) & 1 == 1
}

/// The mask after CPUs `cpus` passed one after another from `p`, those not in `all` changing nothing.
pub open spec fn fill(p: u64, all: u64, cpus: Seq<u32>) -> u64
    decreases cpus.len(),
{
    if cpus.len() == 0 {
        p
    } else {
        let c = cpus[0];
        fill(if has(all, c) { p | (1u64 << (c as u64)) } else { p }, all, cpus.drop_first())
    }
}

/// Every CPU in `all` passed: what ends a grace period.
pub open spec fn full(p: u64, all: u64) -> bool {
    p & all == all
}

#[verifier::bit_vector]
proof fn bit_or(p: u64, c: u64, d: u64)
    requires c < 64, d < 64,
    ensures ((p | (1u64 << c)) >> d) & 1 == 1 <==> ((p >> d) & 1 == 1 || d == c),
{
}

#[verifier::bit_vector]
proof fn bit_of(m: u64, c: u64)
    requires c < 64,
    ensures (m & (1u64 << c) == 0) <==> !((m >> c) & 1 == 1),
{
}

#[verifier::bit_vector]
proof fn bit_zero(d: u64)
    requires d < 64,
    ensures !((0u64 >> d) & 1 == 1),
{
}

#[verifier::bit_vector]
proof fn bit_full(p: u64, all: u64, d: u64)
    requires d < 64, p & all == all, (all >> d) & 1 == 1,
    ensures (p >> d) & 1 == 1,
{
}

#[verifier::bit_vector]
proof fn full_top(p: u64, all: u64)
    requires (all >> 63u64) & 1 == 1 ==> (p >> 63u64) & 1 == 1,
    ensures (p >> 63u64) & (all >> 63u64) == all >> 63u64,
{
}

#[verifier::bit_vector]
proof fn full_step(p: u64, all: u64, k: u64)
    requires
        k < 63,
        (all >> k) & 1 == 1 ==> (p >> k) & 1 == 1,
        (p >> (k + 1)) & (all >> (k + 1)) == all >> (k + 1),
    ensures (p >> k) & (all >> k) == all >> k,
{
}

#[verifier::bit_vector]
proof fn shift_zero(m: u64)
    ensures m >> 0u64 == m,
{
}

/// The bits of `all` from `k` up are in `p`.
proof fn full_from(p: u64, all: u64, k: u64)
    requires k <= 63, forall|c: u32| k <= c < 64 && has(all, c) ==> has(p, c),
    ensures (p >> k) & (all >> k) == all >> k,
    decreases 63 - k,
{
    if k == 63 {
        assert(has(all, 63u32) ==> has(p, 63u32));
        full_top(p, all);
    } else {
        full_from(p, all, (k + 1) as u64);
        assert(has(all, k as u32) ==> has(p, k as u32));
        full_step(p, all, k);
    }
}

/// A mask holding every CPU of `all` is full.
pub proof fn full_if_all(p: u64, all: u64)
    requires forall|c: u32| has(all, c) ==> has(p, c),
    ensures full(p, all),
{
    full_from(p, all, 0);
    shift_zero(p);
    shift_zero(all);
}

/// Which CPUs are in the mask after `cpus` passed: those already in it, and those of `all` among `cpus`.
pub proof fn fill_has(p: u64, all: u64, cpus: Seq<u32>, d: u32)
    ensures has(fill(p, all, cpus), d) <==> (has(p, d) || (has(all, d) && exists|i: int| 0 <= i < cpus.len() && cpus[i] == d)),
    decreases cpus.len(),
{
    if cpus.len() > 0 {
        let c = cpus[0];
        let q = if has(all, c) { p | (1u64 << (c as u64)) } else { p };
        fill_has(q, all, cpus.drop_first(), d);
        if d < 64 && has(all, c) {
            bit_or(p, c as u64, d as u64);
        }
        if exists|i: int| 0 <= i < cpus.len() && cpus[i] == d {
            let i = choose|i: int| 0 <= i < cpus.len() && cpus[i] == d;
            if i > 0 {
                assert(cpus.drop_first()[i - 1] == d);
            }
        }
        if exists|i: int| 0 <= i < cpus.drop_first().len() && cpus.drop_first()[i] == d {
            let i = choose|i: int| 0 <= i < cpus.drop_first().len() && cpus.drop_first()[i] == d;
            assert(cpus[i + 1] == d);
        }
    }
}

/// Progress: when every CPU in `all` passes a quiescent point after a grace period starts (`passed` empty), in any order
/// and with any repeats or other CPUs in between, the mask is full, so [`Monitor::quiescent`] ends that grace period at
/// the latest at the last of them and returns its items (its `ensures`).
pub proof fn round_ends(all: u64, cpus: Seq<u32>)
    requires forall|c: u32| #[trigger] has(all, c) ==> exists|i: int| 0 <= i < cpus.len() && cpus[i] == c,
    ensures full(fill(0, all, cpus), all),
{
    assert forall|c: u32| #[trigger] has(all, c) implies has(fill(0, all, cpus), c) by {
        fill_has(0, all, cpus, c);
    }
    full_if_all(fill(0, all, cpus), all);
}

impl<T> Monitor<T> {
    /// The running grace period's items.
    pub closed spec fn current(&self) -> Seq<T> {
        self.current@
    }

    /// The items waiting for the next grace period.
    pub closed spec fn next(&self) -> Seq<T> {
        self.next@
    }

    /// The items whose grace period ended, not handed out yet.
    pub closed spec fn ready(&self) -> Seq<T> {
        self.ready@
    }

    pub closed spec fn passed(&self) -> u64 {
        self.passed
    }

    pub closed spec fn monitoring(&self) -> bool {
        self.monitoring
    }

    pub closed spec fn complete(&self) -> bool {
        self.complete
    }

    pub closed spec fn now(&self) -> nat {
        self.history@.now
    }

    pub closed spec fn start(&self) -> nat {
        self.history@.start
    }

    pub closed spec fn retired(&self) -> Seq<T> {
        self.history@.retired
    }

    pub closed spec fn retired_at(&self, i: int) -> nat {
        self.history@.at[i]
    }

    pub closed spec fn returned(&self) -> nat {
        self.history@.returned
    }

    pub closed spec fn ready_start(&self) -> nat {
        self.history@.ready_start
    }

    pub closed spec fn ready_end(&self) -> nat {
        self.history@.ready_end
    }

    pub closed spec fn ready_gp(&self) -> nat {
        self.history@.ready_gp
    }

    pub closed spec fn ready_all(&self) -> u64 {
        self.history@.ready_all
    }

    pub closed spec fn last_qs(&self, c: u32) -> nat {
        self.history@.last_qs[c as int]
    }

    /// What holds between calls.
    pub closed spec fn inv(&self) -> bool {
        let h = self.history@;
        let r = h.returned as int;
        let w = r + self.current@.len();
        &&& self.current.wf()
        &&& self.next.wf()
        &&& self.ready.wf()
        &&& h.last_qs.len() == 64
        &&& h.at.len() == h.retired.len()
        &&& w + self.next@.len() == h.retired.len()
        &&& self.current@ == h.retired.subrange(r, w)
        &&& self.next@ == h.retired.subrange(w, h.retired.len() as int)
        &&& h.ready_start <= h.ready_end <= h.returned
        &&& self.ready@ == h.retired.subrange(h.ready_start as int, h.ready_end as int)
        &&& forall|i: int| h.ready_start <= i < h.ready_end ==> h.at[i] <= h.ready_gp
        &&& h.ready_start < h.ready_end ==> forall|c: u32| #[trigger] has(h.ready_all, c) ==> h.last_qs[c as int] > h.ready_gp
        &&& h.start <= h.now
        &&& forall|i: int| 0 <= i < h.at.len() ==> h.at[i] <= h.now
        &&& forall|i: int| r <= i < w ==> h.at[i] <= h.start
        &&& forall|c: int| 0 <= c < 64 ==> h.last_qs[c] <= h.now
        &&& self.monitoring == (!self.complete || self.ready@.len() > 0)
        &&& self.complete ==> self.current@.len() == 0 && self.next@.len() == 0
        &&& !self.complete ==> forall|c: u32| #[trigger] has(self.passed, c) ==> h.last_qs[c as int] > h.start
    }
}

/// No loss: everything retired is, in order, what was handed out, the ready items, what was handed out of the ready
/// ones (they go newest first), the running grace period's items, then the waiting ones.
pub proof fn no_loss<T>(m: &Monitor<T>)
    requires m.inv(),
    ensures
        m.retired() == m.retired().subrange(0, m.ready_start() as int) + m.ready()
            + m.retired().subrange(m.ready_end() as int, m.returned() as int) + m.current() + m.next(),
{
    assert(m.retired() =~= m.retired().subrange(0, m.ready_start() as int) + m.ready()
        + m.retired().subrange(m.ready_end() as int, m.returned() as int) + m.current() + m.next());
}

const fn fresh<T>() -> (h: Ghost<History<T>>)
    ensures
        h@.now == 0,
        h@.start == 0,
        h@.retired.len() == 0,
        h@.at.len() == 0,
        h@.returned == 0,
        h@.ready_start == 0,
        h@.ready_end == 0,
        h@.last_qs == Seq::new(64, |c: int| 0nat),
{
    Ghost(History {
        now: 0,
        start: 0,
        retired: Seq::empty(),
        at: Seq::empty(),
        returned: 0,
        ready_start: 0,
        ready_end: 0,
        ready_gp: 0,
        ready_all: 0,
        last_qs: Seq::new(64, |c: int| 0nat),
    })
}

} // verus!

#[cfg_attr(verus_keep_ghost, verus_verify)]
impl<T> Monitor<T> {
    /// No grace period running, nothing retired (Asterinas's `GracePeriod::new`: complete, not monitoring).
    #[cfg_attr(verus_keep_ghost, verus_spec(m =>
        ensures
            m.inv(),
            !m.monitoring(),
            m.retired().len() == 0,
    ))]
    pub const fn new() -> Self {
        let m = Monitor {
            current: Queue::new(),
            next: Queue::new(),
            ready: Queue::new(),
            passed: 0,
            complete: true,
            monitoring: false,
            #[cfg(verus_keep_ghost)]
            history: fresh(),
        };
        #[cfg(verus_keep_ghost)]
        proof! {
            assert(m.current@ =~= m.history@.retired.subrange(0, 0));
            assert(m.next@ =~= m.history@.retired.subrange(0, 0));
            assert(m.ready@ =~= m.history@.retired.subrange(0, 0));
        }
        m
    }

    /// Whether there is anything to do: a grace period runs, or items wait to be handed out. When not,
    /// [`Monitor::quiescent`] and [`Monitor::take_ready`] have nothing to do.
    #[cfg_attr(verus_keep_ghost, verus_spec(r =>
        ensures r == self.monitoring()
    ))]
    pub fn is_monitoring(&self) -> bool {
        self.monitoring
    }

    /// Starts a grace period for the waiting items (Asterinas's `restart(take(next_callbacks))`).
    #[cfg_attr(verus_keep_ghost, verus_spec(
        requires
            old(self).current@.len() == 0,
        ensures
            final(self).current@ == old(self).next@,
            final(self).next@.len() == 0,
            final(self).current.wf() == old(self).next.wf(),
            final(self).next.wf() == old(self).current.wf(),
            final(self).ready@ == old(self).ready@,
            final(self).ready.wf() == old(self).ready.wf(),
            final(self).passed == 0,
            !final(self).complete,
            final(self).monitoring == old(self).monitoring,
            final(self).history@ == (History { start: old(self).history@.now, ..old(self).history@ }),
    ))]
    fn restart(&mut self) {
        #[cfg(verus_keep_ghost)]
        proof! { self.history@ = History { start: self.history@.now, ..self.history@ }; }
        core::mem::swap(&mut self.current, &mut self.next);
        self.passed = 0;
        self.complete = false;
    }

    /// Frees `item` after a grace period (Asterinas's `after_grace_period`): it waits for the next one, which starts now
    /// if none runs.
    #[cfg_attr(verus_keep_ghost, verus_spec(
        requires
            old(self).inv(),
        ensures
            final(self).inv(),
            final(self).now() == old(self).now() + 1,
            final(self).retired() == old(self).retired().push(item),
            final(self).retired_at(old(self).retired().len() as int) == final(self).now(),
            final(self).returned() == old(self).returned(),
            final(self).ready() == old(self).ready(),
            final(self).ready_end() == old(self).ready_end(),
            final(self).monitoring(),
            !old(self).complete() ==> final(self).current() == old(self).current()
                && final(self).next() == old(self).next().push(item)
                && final(self).passed() == old(self).passed()
                && final(self).start() == old(self).start(),
            old(self).complete() ==> final(self).current() == seq![item]
                && final(self).next().len() == 0
                && final(self).passed() == 0
                && final(self).start() == final(self).now(),
    ))]
    pub fn retire(&mut self, item: T) {
        #[cfg(verus_keep_ghost)]
        proof! {
            let now = self.history@.now + 1;
            self.history@ = History {
                now,
                retired: self.history@.retired.push(item),
                at: self.history@.at.push(now),
                ..self.history@
            };
            let r = self.history@.returned as int;
            let w = r + self.current@.len();
            assert(self.current@ =~= self.history@.retired.subrange(r, w));
            assert(self.next@.push(item) =~= self.history@.retired.subrange(w, self.history@.retired.len() as int));
            assert(self.ready@ =~= self.history@.retired.subrange(self.history@.ready_start as int, self.history@.ready_end as int));
        }
        self.next.push(item);
        if !self.complete {
            return;
        }
        self.restart();
        self.monitoring = true;
        #[cfg(verus_keep_ghost)]
        proof! {
            assert forall|c: u32| #[trigger] has(self.passed, c) implies self.history@.last_qs[c as int] > self.history@.start by {
                if c < 64 { bit_zero(c as u64); }
            }
            let n = self.history@.retired.len() as int;
            assert(self.next@ =~= self.history@.retired.subrange(n, n));
        }
    }

    /// CPU `cpu` passed a quiescent point (Asterinas's `finish_grace_period`); `all` is the mask of CPUs that must pass.
    /// When that ends the running grace period, its items become ready, for [`Monitor::take_ready`] to hand out a chunk at
    /// a time. A grace period whose CPUs have all passed ends only once the ready items before it are all out (Linux's
    /// `rcu_do_batch` likewise runs a bounded batch per pass): the CPUs keep passing, and each pass hands out another chunk.
    /// A `cpu` not in `all` (or 64 and up) changes nothing.
    #[cfg_attr(verus_keep_ghost, verus_spec(
        requires
            old(self).inv(),
        ensures
            final(self).inv(),
            final(self).now() == old(self).now() + 1,
            final(self).retired() == old(self).retired(),
            final(self).ready_start() <= final(self).ready_end(),
            has(all, cpu) ==> final(self).last_qs(cpu) == final(self).now(),
            // Nothing to do, or a CPU that does not count.
            !old(self).monitoring() || old(self).complete() || !has(all, cpu) ==>
                final(self).current() == old(self).current()
                && final(self).next() == old(self).next()
                && final(self).ready() == old(self).ready()
                && final(self).ready_end() == old(self).ready_end()
                && final(self).returned() == old(self).returned()
                && final(self).passed() == old(self).passed()
                && final(self).monitoring() == old(self).monitoring()
                && final(self).start() == old(self).start(),
            // A CPU of `all` passes: it adds to the grace period, which goes on while any CPU is missing or items are ready.
            old(self).monitoring() && !old(self).complete() && has(all, cpu)
                && (!full(old(self).passed() | (1u64 << (cpu as u64)), all) || old(self).ready().len() > 0) ==>
                final(self).current() == old(self).current()
                && final(self).next() == old(self).next()
                && final(self).ready() == old(self).ready()
                && final(self).ready_end() == old(self).ready_end()
                && final(self).returned() == old(self).returned()
                && final(self).passed() == old(self).passed() | (1u64 << (cpu as u64))
                && final(self).monitoring()
                && final(self).start() == old(self).start(),
            // Every CPU of `all` passed and nothing is ready: the running grace period's items become ready, and the waiting
            // ones start the next.
            old(self).monitoring() && !old(self).complete() && has(all, cpu)
                && full(old(self).passed() | (1u64 << (cpu as u64)), all) && old(self).ready().len() == 0 ==>
                final(self).ready() == old(self).current()
                && final(self).ready_end() == final(self).returned()
                && final(self).returned() == old(self).returned() + old(self).current().len()
                && final(self).ready_gp() == old(self).start()
                && final(self).ready_all() == all
                // Safety: every CPU of `all` passed since that grace period started.
                && (forall|c: u32| #[trigger] has(all, c) ==> final(self).last_qs(c) > old(self).start())
                && final(self).current() == old(self).next()
                && final(self).next().len() == 0
                && final(self).monitoring() == (old(self).next().len() > 0 || old(self).current().len() > 0)
                && (old(self).next().len() > 0 ==> final(self).passed() == 0 && final(self).start() == final(self).now()),
    ))]
    pub fn quiescent(&mut self, cpu: u32, all: u64) {
        #[cfg(verus_keep_ghost)]
        proof! {
            let now = self.history@.now + 1;
            let last_qs = if has(all, cpu) { self.history@.last_qs.update(cpu as int, now) } else { self.history@.last_qs };
            self.history@ = History { now, last_qs, ..self.history@ };
        }
        if !self.monitoring || self.complete {
            return;
        }
        #[cfg(verus_keep_ghost)]
        proof! { if cpu < 64 { bit_of(all, cpu as u64); } }
        if cpu >= 64 || all & (1u64 << cpu) == 0 {
            return;
        }
        #[cfg(verus_keep_ghost)]
        let old_passed = self.passed;
        self.passed |= 1u64 << cpu;
        #[cfg(verus_keep_ghost)]
        proof! {
            assert forall|c: u32| #[trigger] has(self.passed, c) implies self.history@.last_qs[c as int] > self.history@.start by {
                bit_or(old_passed, cpu as u64, c as u64);
                if c != cpu {
                    assert(has(old_passed, c));
                }
            }
        }
        if self.passed & all != all {
            return;
        }
        // The ready items of the grace period before go out first: this one's become ready only when they are.
        if !self.ready.is_empty() {
            return;
        }
        self.complete = true;
        core::mem::swap(&mut self.ready, &mut self.current);
        #[cfg(verus_keep_ghost)]
        proof! {
            assert forall|c: u32| #[trigger] has(all, c) implies self.history@.last_qs[c as int] > self.history@.start by {
                bit_full(self.passed, all, c as u64);
                assert(has(self.passed, c));
            }
            let r = self.history@.returned;
            let len = self.ready@.len();
            self.history@ = History {
                returned: r + len,
                ready_start: r,
                ready_end: r + len,
                ready_gp: self.history@.start,
                ready_all: all,
                ..self.history@
            };
            assert(self.ready@ =~= self.history@.retired.subrange(r as int, (r + len) as int));
            assert(self.next@ =~= self.history@.retired.subrange(self.history@.returned as int, self.history@.retired.len() as int));
            assert(self.current@ =~= self.history@.retired.subrange(self.history@.returned as int, self.history@.returned as int));
        }
        if !self.next.is_empty() {
            self.restart();
            #[cfg(verus_keep_ghost)]
            proof! {
                assert forall|c: u32| #[trigger] has(self.passed, c) implies self.history@.last_qs[c as int] > self.history@.start by {
                    if c < 64 { bit_zero(c as u64); }
                }
                let n = self.history@.retired.len() as int;
                assert(self.next@ =~= self.history@.retired.subrange(n, n));
            }
        } else {
            self.monitoring = !self.ready.is_empty();
        }
    }

    /// Hands out ready items, at most [`CHUNK`] at a time and the newest first: they are to be freed now. Empty when
    /// none are ready. What it returns was retired by the start of a grace period that every CPU of its `all` passed.
    #[cfg_attr(verus_keep_ghost, verus_spec(r =>
        requires
            old(self).inv(),
        ensures
            final(self).inv(),
            final(self).now() == old(self).now(),
            final(self).retired() == old(self).retired(),
            final(self).returned() == old(self).returned(),
            final(self).ready_end() == old(self).ready_end() - r@.len(),
            r@ == old(self).retired().subrange(final(self).ready_end() as int, old(self).ready_end() as int),
            r@.len() <= CHUNK,
            (r@.len() == 0) == (old(self).ready().len() == 0),
            // Safety.
            forall|i: int| final(self).ready_end() <= i < old(self).ready_end() ==> final(self).retired_at(i) <= old(self).ready_gp(),
            r@.len() > 0 ==> forall|c: u32| #[trigger] has(old(self).ready_all(), c) ==> final(self).last_qs(c) > old(self).ready_gp(),
            // Nothing else changes.
            final(self).current() == old(self).current(),
            final(self).next() == old(self).next(),
            final(self).passed() == old(self).passed(),
            final(self).start() == old(self).start(),
            final(self).complete() == old(self).complete(),
            final(self).monitoring() == (!old(self).complete() || final(self).ready().len() > 0),
    ))]
    pub fn take_ready(&mut self) -> Vec<T> {
        let out = self.ready.take_last();
        #[cfg(verus_keep_ghost)]
        proof! {
            let rs = self.history@.ready_start as int;
            let re = self.history@.ready_end as int;
            let k = out@.len() as int;
            assert(old(self).ready@ == self.ready@ + out@);
            assert(self.ready@ =~= self.history@.retired.subrange(rs, re - k));
            assert forall|j: int| 0 <= j < k implies out@[j] == self.history@.retired[re - k + j] by {
                assert(out@[j] == old(self).ready@[self.ready@.len() + j]);
            }
            assert(out@ =~= self.history@.retired.subrange(re - k, re));
            self.history@ = History { ready_end: (re - k) as nat, ..self.history@ };
        }
        if self.complete && self.ready.is_empty() {
            self.monitoring = false;
        }
        out
    }
}

#[cfg(not(verus_keep_ghost))]
impl<T> Default for Monitor<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests;
