// Verus proof of the SHIPPED code, not a model (project rule D-011): this file includes
// drivers/lucina-audio-ledger/src/lib.rs by #[path], so Verus checks the functions as the kernel compiles them; what each
// promises is in its `ensures`. On top of those, this file proves:
// - the FIFO: the slots out are the sequence oldest, oldest + 1, ... round the ring, all distinct; `queue` appends a slot that
//   is not already out and `done` accepts exactly the head of that sequence, so every period is handed out once and comes
//   back once, in the order it went;
// - the samples: a slot's data bit says whether the period queued in it held samples, until it comes back;
// - the undo: the steps `undo_step` gives for i < taken are each of the steps taken, exactly once.
#![feature(proc_macro_hygiene)]
#[path = "../../drivers/lucina-audio-ledger/src/lib.rs"]
pub mod ledger;

use ledger::*;
use vstd::prelude::*;

verus! {

/// The slots out, oldest first.
pub open spec fn outs(l: Ledger, p: u32) -> Seq<int> {
    Seq::new(l.count as nat, |i: int| slot_at(l.oldest, i, p))
}

/// Two places in the ring less than a full turn apart are different slots.
proof fn lemma_distinct(oldest: u32, p: u32, i: int, j: int)
    requires
        oldest < p,
        0 <= i < j < p,
    ensures
        slot_at(oldest, i, p) != slot_at(oldest, j, p),
        0 <= slot_at(oldest, i, p) < p,
{
}

/// A place in the ring is a slot.
proof fn lemma_range(oldest: u32, p: u32, i: int)
    requires
        oldest < p,
        0 <= i < p,
    ensures
        0 <= slot_at(oldest, i, p) < p,
{
}

/// The slots out are distinct and in range.
proof fn lemma_outs(l: Ledger, p: u32)
    requires
        ok(l, p),
    ensures
        forall|i: int, j: int| 0 <= i < j < l.count ==> outs(l, p)[i] != outs(l, p)[j],
        forall|i: int| 0 <= i < l.count ==> 0 <= #[trigger] outs(l, p)[i] < p,
{
    assert forall|i: int, j: int| 0 <= i < j < l.count implies outs(l, p)[i] != outs(l, p)[j] by {
        lemma_distinct(l.oldest, p, i, j);
    }
    assert forall|i: int| 0 <= i < l.count implies 0 <= #[trigger] outs(l, p)[i] < p by {
        lemma_range(l.oldest, p, i);
    }
}

/// A slot is out exactly when it is in the sequence.
proof fn lemma_out_is_in(l: Ledger, p: u32, s: u32)
    requires
        ok(l, p),
        s < p,
    ensures
        out(l, s, p) == outs(l, p).contains(s as int),
{
    if out(l, s, p) {
        let i = dist(l.oldest, s, p);
        assert(outs(l, p)[i] == s as int);
    }
    if outs(l, p).contains(s as int) {
        let i = choose|i: int| 0 <= i < outs(l, p).len() && outs(l, p)[i] == s as int;
        assert(dist(l.oldest, s, p) == i);
    }
}

/// Queueing appends one slot that was not out.
fn queue_appends(l: Ledger, p: u32, has_data: bool) -> (r: Ledger)
    requires
        ok(l, p),
        l.count < p,
    ensures
        ok(r, p),
        !outs(l, p).contains(nxt(l, p) as int),
        outs(r, p) == outs(l, p).push(nxt(l, p) as int),
        bit(r.data, nxt(l, p)) == has_data,
{
    let s = next_slot(l, p);
    let r = queue(l, p, has_data);
    proof {
        lemma_out_is_in(l, p, s);
        assert(outs(r, p) =~= outs(l, p).push(s as int));
    }
    r
}

/// Taking a slot back succeeds exactly for the oldest out, and removes it from the front.
fn done_pops(l: Ledger, p: u32, slot: u32) -> (r: Option<(Ledger, bool)>)
    requires
        ok(l, p),
    ensures
        r.is_some() == (outs(l, p).len() > 0 && outs(l, p)[0] == slot as int),
        r matches Some((n, had)) ==> ok(n, p) && outs(n, p) == outs(l, p).drop_first() && had == bit(l.data, slot),
{
    let r = done(l, p, slot);
    proof {
        if l.count > 0 {
            assert(outs(l, p)[0] == l.oldest as int);
        }
        if let Some((n, had)) = r {
            assert forall|i: int| 0 <= i < n.count implies #[trigger] outs(n, p)[i] == outs(l, p).drop_first()[i] by {
                assert(slot_at(n.oldest, i, p) == slot_at(l.oldest, i + 1, p));
            }
            assert(outs(n, p) =~= outs(l, p).drop_first());
        }
    }
    r
}

/// A whole stream, any interleaving of hand-outs and answers: what goes out comes back once each, in order. `went` is every
/// slot handed out so far, `back` how many came back; the slots out are exactly the ones not yet back, in order.
pub open spec fn fifo(l: Ledger, p: u32, went: Seq<int>, back: int) -> bool {
    &&& ok(l, p)
    &&& 0 <= back <= went.len()
    &&& outs(l, p) == went.subrange(back, went.len() as int)
}

fn fifo_queue(l: Ledger, p: u32, has_data: bool, Ghost(went): Ghost<Seq<int>>, Ghost(back): Ghost<int>) -> (r: Ledger)
    requires
        fifo(l, p, went, back),
        l.count < p,
    ensures
        fifo(r, p, went.push(nxt(l, p) as int), back),
{
    let r = queue_appends(l, p, has_data);
    proof {
        assert(went.push(nxt(l, p) as int).subrange(back, went.len() as int + 1) =~= went.subrange(back, went.len() as int).push(nxt(l, p) as int));
    }
    r
}

fn fifo_done(l: Ledger, p: u32, slot: u32, Ghost(went): Ghost<Seq<int>>, Ghost(back): Ghost<int>) -> (r: Option<(Ledger, bool)>)
    requires
        fifo(l, p, went, back),
    ensures
        r.is_some() == (back < went.len() && went[back] == slot as int),
        r matches Some((n, _)) ==> fifo(n, p, went, back + 1),
{
    let r = done_pops(l, p, slot);
    proof {
        if back < went.len() {
            assert(outs(l, p)[0] == went[back]);
        }
        if let Some((n, _)) = r {
            assert(went.subrange(back + 1, went.len() as int) =~= went.subrange(back, went.len() as int).drop_first());
        }
    }
    r
}

/// The undo steps for i < taken are a permutation of the steps taken: in range, and no step twice.
proof fn undo_once(order: Undo, taken: u32, i: u32, j: u32)
    requires
        i < taken,
        j < taken,
        i != j,
    ensures
        ({
            let a = if order == Undo::Reverse { taken - 1 - i } else { i as int };
            let b = if order == Undo::Reverse { taken - 1 - j } else { j as int };
            0 <= a < taken && a != b
        }),
{
}

fn undo_is_permutation(order: Undo, taken: u32, i: u32, j: u32) -> (r: (u32, u32))
    requires
        i < taken,
        j < taken,
        i != j,
    ensures
        r.0 < taken,
        r.1 < taken,
        r.0 != r.1,
{
    (undo_step(order, taken, i), undo_step(order, taken, j))
}

/// Every step taken is undone by some i: the map is onto.
fn undo_covers(order: Undo, taken: u32, step: u32) -> (i: u32)
    requires
        step < taken,
    ensures
        i < taken,
        undo_step_spec(order, taken, i) == step,
{
    match order {
        Undo::Reverse => taken - 1 - step,
        Undo::AsTaken => step,
    }
}

pub open spec fn undo_step_spec(order: Undo, taken: u32, i: u32) -> int {
    if order == Undo::Reverse { taken - 1 - i } else { i as int }
}

/// The shipped undo_step is undo_step_spec.
fn undo_step_matches(order: Undo, taken: u32, i: u32) -> (r: u32)
    requires
        i < taken,
    ensures
        r as int == undo_step_spec(order, taken, i),
{
    undo_step(order, taken, i)
}

/// The microphone queue: pushing what fits drops nothing, and a full queue keeps exactly `cap`.
fn mic_cases(held: usize, new: usize, cap: usize)
    requires
        held <= cap,
        held + new <= usize::MAX,
{
    let (keep, dropped) = mic_push(held, new, cap);
    if held + new <= cap {
        assert(dropped == 0 && keep == held + new);
    } else {
        assert(keep == cap && dropped == held + new - cap);
    }
}

} // verus!

fn main() {}
