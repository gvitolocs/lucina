// The flight recorder's sequence rules: what a reader that follows a numbered log must conclude from the next number it sees.
// Verus checks this very file: `//@` lines hold the proof, see scripts/verify-shipped.sh (D-011); the plain functions are what
// ships. lib.rs and both programs call these functions and hold no copy of the rules.
//
// Sequence numbers are the kernel log's (`/dev/kmsg`, kernel/lucina-klog): they count up by one per record and start at 0.
// A reader that was overtaken by the ring, or one that came back after a lost connection, sees a number above the one it
// expected; the difference is how many records it never saw, and it must say so instead of staying silent.
//
// After Linux's /dev/kmsg (EPIPE for an overtaken reader) and systemd-journald's "Missed N messages" line.

/// What the next sequence number means, given the one expected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Seq {
    /// Exactly the one expected.
    InOrder,
    /// Records `from..to` (`to` excluded) were never seen; the new one is `to`.
    Gap { from: u64, to: u64 },
    /// Lower than expected: a duplicate after a reconnect (to be dropped) or a log that restarted.
    Behind,
}

/// Judges sequence number `seq` when `expected` was due.
pub fn judge(expected: u64, seq: u64) -> Seq //@ret
//@ ensures
//@     // The three cases cover every input, so the answer is fixed for each: equal is in order, lower is behind, higher is a
//@     // gap from exactly the number that was due to exactly the one that came.
//@     seq == expected ==> ret == Seq::InOrder,
//@     seq < expected ==> ret == Seq::Behind,
//@     seq > expected ==> ret == (Seq::Gap { from: expected, to: seq }),
{
    if seq == expected {
        Seq::InOrder
    } else if seq < expected {
        Seq::Behind
    } else {
        Seq::Gap {
            from: expected,
            to: seq,
        }
    }
}

//@ // The number due after `seq`: one higher, and `u64::MAX` at the top (no wrap to 0).
//@ pub open spec fn due_after(seq: u64) -> u64 { if seq < u64::MAX { (seq + 1) as u64 } else { u64::MAX } }

/// The number due after `seq` was taken: one higher, and it stays put at the top of the range instead of wrapping.
pub fn after(seq: u64) -> u64 //@ret
//@ ensures
//@     ret == due_after(seq),
{
    if seq < u64::MAX { seq + 1 } else { u64::MAX }
}

/// What taking a record decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Taken {
    /// Keep it. `lost` is `Some((from, to))` when records before it were missed.
    Keep { lost: Option<(u64, u64)> },
    /// Already seen (a replay after a reconnect): drop it.
    Duplicate,
}

//@ // One cursor step, written out: the record due is kept and the cursor moves past it; a higher one is kept, the numbers
//@ // from the one due up to it (excluded) are reported lost, and the cursor moves past it; a lower one is a duplicate, dropped,
//@ // and nothing moves.
//@ pub open spec fn step(expected: u64, seq: u64) -> (u64, Taken) {
//@     if seq == expected {
//@         (due_after(seq), Taken::Keep { lost: None })
//@     } else if seq > expected {
//@         (due_after(seq), Taken::Keep { lost: Some((expected, seq)) })
//@     } else {
//@         (expected, Taken::Duplicate)
//@     }
//@ }

//@ // Whether taking record `seq` with the answer `t` accounts for number `n`: `n` is the record kept, or lies in the range
//@ // reported lost. A duplicate accounts for nothing.
//@ pub open spec fn reports(t: Taken, seq: u64, n: u64) -> bool {
//@     match t {
//@         Taken::Keep { lost: None } => n == seq,
//@         Taken::Keep { lost: Some((from, to)) } => n == seq || (from <= n && n < to),
//@         Taken::Duplicate => false,
//@     }
//@ }

/// One step of a cursor that expects `expected` and was handed `seq`: the number due afterwards, and what to do with the record.
pub fn take_step(expected: u64, seq: u64) -> (u64, Taken) //@ret
//@ ensures
//@     // Both outputs, for every input: the step above, nothing else.
//@     ret == step(expected, seq),
//@     // What is due next never goes back.
//@     ret.0 >= expected,
//@     // What this one record accounts for (the record kept and the range reported lost) is exactly the numbers from the one
//@     // that was due up to the one due next: nothing in between is passed over in silence, and nothing below `expected` is
//@     // counted again. The record numbered u64::MAX is the exception: it is kept and stays due (see lemma_once_and_complete).
//@     seq < u64::MAX ==> forall|n: u64| #[trigger] reports(ret.1, seq, n) <==> (expected <= n && n < ret.0),
{
    match judge(expected, seq) {
        Seq::InOrder => (after(seq), Taken::Keep { lost: None }),
        Seq::Gap { from, to } => (
            after(seq),
            Taken::Keep {
                lost: Some((from, to)),
            },
        ),
        Seq::Behind => (expected, Taken::Duplicate),
    }
}

/// The number due after records up to `to` (excluded) are known lost: `to` if that is further on, else unchanged.
pub fn skip_step(expected: u64, to: u64) -> u64 //@ret
//@ ensures
//@     // Exactly the larger of the two: never back, past every number below `to`, and no further than that.
//@     ret >= expected,
//@     ret >= to,
//@     ret == expected || ret == to,
{
    if to > expected { to } else { expected }
}

//@ // A cursor's whole life is a run of operations: `(true, seq)` is take_step with record `seq`, `(false, to)` is skip_step to
//@ // `to` (the other end said the numbers below `to` are lost). These give what is due after one, and what one accounts for: a
//@ // take as above, a skip the numbers it moves the cursor past.
//@ pub open spec fn op_due(expected: u64, op: (u64, bool)) -> u64 {
//@     if op.1 { step(expected, op.0).0 } else if op.0 > expected { op.0 } else { expected }
//@ }
//@ pub open spec fn op_reports(expected: u64, op: (u64, bool), n: u64) -> bool {
//@     if op.1 { reports(step(expected, op.0).1, op.0, n) } else { expected <= n && n < op_due(expected, op) }
//@ }
//@ // What is due before operation `i` of `ops` (after the first `i`), from `start`.
//@ pub open spec fn due_at(start: u64, ops: vstd::seq::Seq<(u64, bool)>, i: nat) -> u64
//@     decreases i,
//@ {
//@     if i == 0 || i > ops.len() { start } else { op_due(due_at(start, ops, (i - 1) as nat), ops[i - 1]) }
//@ }
//@ // Operation `i` of the run accounts for number `n`.
//@ pub open spec fn reported_by(start: u64, ops: vstd::seq::Seq<(u64, bool)>, i: nat, n: u64) -> bool {
//@     op_reports(due_at(start, ops, i), ops[i as int], n)
//@ }
//@ // Some operation of the run accounts for number `n`.
//@ pub open spec fn accounted(start: u64, ops: vstd::seq::Seq<(u64, bool)>, n: u64) -> bool {
//@     exists|i: nat| i < ops.len() && #[trigger] reported_by(start, ops, i, n)
//@ }
//@ // No take of the record numbered u64::MAX (2^64 records away): that record is kept and stays due, so it would be kept again.
//@ pub open spec fn below_top(ops: vstd::seq::Seq<(u64, bool)>) -> bool {
//@     forall|i: int| 0 <= i < ops.len() ==> !(#[trigger] ops[i]).1 || ops[i].0 < u64::MAX
//@ }

//@ proof fn lemma_op(e: u64, op: (u64, bool))
//@     ensures
//@         e <= op_due(e, op),
//@         !op.1 || op.0 < u64::MAX ==> forall|n: u64| #[trigger] op_reports(e, op, n) <==> (e <= n && n < op_due(e, op)),
//@ {
//@ }

//@ proof fn lemma_due_next(start: u64, ops: vstd::seq::Seq<(u64, bool)>, i: nat)
//@     requires
//@         i < ops.len(),
//@     ensures
//@         due_at(start, ops, i + 1) == op_due(due_at(start, ops, i), ops[i as int]),
//@         due_at(start, ops, i) <= due_at(start, ops, i + 1),
//@ {
//@     lemma_op(due_at(start, ops, i), ops[i as int]);
//@ }

//@ proof fn lemma_mono(start: u64, ops: vstd::seq::Seq<(u64, bool)>, i: nat, j: nat)
//@     requires
//@         i <= j <= ops.len(),
//@     ensures
//@         due_at(start, ops, i) <= due_at(start, ops, j),
//@     decreases j - i,
//@ {
//@     if i < j {
//@         lemma_mono(start, ops, i, (j - 1) as nat);
//@         lemma_due_next(start, ops, (j - 1) as nat);
//@     }
//@ }

//@ proof fn lemma_interval(start: u64, ops: vstd::seq::Seq<(u64, bool)>, i: nat, n: u64)
//@     requires
//@         i < ops.len(),
//@         below_top(ops),
//@     ensures
//@         reported_by(start, ops, i, n) <==> (due_at(start, ops, i) <= n && n < due_at(start, ops, i + 1)),
//@ {
//@     lemma_due_next(start, ops, i);
//@     lemma_op(due_at(start, ops, i), ops[i as int]);
//@     assert(!ops[i as int].1 || ops[i as int].0 < u64::MAX);
//@ }

//@ proof fn lemma_complete(start: u64, ops: vstd::seq::Seq<(u64, bool)>, k: nat, n: u64)
//@     requires
//@         k <= ops.len(),
//@         below_top(ops),
//@         start <= n && n < due_at(start, ops, k),
//@     ensures
//@         exists|i: nat| i < k && #[trigger] reported_by(start, ops, i, n),
//@     decreases k,
//@ {
//@     if k > 0 {
//@         let p = (k - 1) as nat;
//@         lemma_interval(start, ops, p, n);
//@         if n < due_at(start, ops, p) {
//@             lemma_complete(start, ops, p, n);
//@         } else {
//@             assert(reported_by(start, ops, p, n));
//@         }
//@     }
//@ }

//@ // Over any run of takes and skips from `start` (the cursor's whole life), none taking record u64::MAX:
//@ //  - what is due never goes back;
//@ //  - each operation accounts for exactly the numbers from what was due before it to what is due after it;
//@ //  - no number is accounted for by two operations: a lost range or a kept record is reported once, never again;
//@ //  - every number from `start` up to what is due at the end is accounted for by some operation (kept, reported lost by
//@ //    this end, or reported lost by the other end through a skip), and nothing outside that is: no record is missed in silence.
//@ pub proof fn lemma_once_and_complete(start: u64, ops: vstd::seq::Seq<(u64, bool)>)
//@     requires
//@         below_top(ops),
//@     ensures
//@         forall|i: nat, j: nat| i <= j <= ops.len() ==> #[trigger] due_at(start, ops, i) <= #[trigger] due_at(start, ops, j),
//@         forall|i: nat, n: u64| i < ops.len() ==> (#[trigger] reported_by(start, ops, i, n) <==> (due_at(start, ops, i) <= n
//@             && n < due_at(start, ops, i + 1))),
//@         forall|i: nat, j: nat, n: u64| i < j < ops.len() && #[trigger] reported_by(start, ops, i, n) ==> !#[trigger]
//@             reported_by(start, ops, j, n),
//@         forall|n: u64| start <= n && n < due_at(start, ops, ops.len()) ==> #[trigger] accounted(start, ops, n),
//@         forall|i: nat, n: u64| i < ops.len() && #[trigger] reported_by(start, ops, i, n) ==> start <= n && n < due_at(start,
//@             ops, ops.len()),
//@ {
//@     assert forall|i: nat, j: nat| i <= j <= ops.len() implies #[trigger] due_at(start, ops, i) <= #[trigger] due_at(start, ops,
//@         j) by {
//@         lemma_mono(start, ops, i, j);
//@     }
//@     assert forall|i: nat, n: u64| i < ops.len() implies (#[trigger] reported_by(start, ops, i, n) <==> (due_at(start, ops, i) <= n
//@         && n < due_at(start, ops, i + 1))) by {
//@         lemma_interval(start, ops, i, n);
//@     }
//@     assert forall|i: nat, j: nat, n: u64| i < j < ops.len() && #[trigger] reported_by(start, ops, i, n) implies !#[trigger]
//@         reported_by(start, ops, j, n) by {
//@         lemma_interval(start, ops, i, n);
//@         lemma_interval(start, ops, j, n);
//@         lemma_mono(start, ops, i + 1, j);
//@     }
//@     assert forall|n: u64| start <= n && n < due_at(start, ops, ops.len()) implies #[trigger] accounted(start, ops, n) by {
//@         lemma_complete(start, ops, ops.len(), n);
//@     }
//@     assert forall|i: nat, n: u64| i < ops.len() && #[trigger] reported_by(start, ops, i, n) implies start <= n && n < due_at(
//@         start, ops, ops.len()) by {
//@         lemma_interval(start, ops, i, n);
//@         lemma_mono(start, ops, 0, i);
//@         lemma_mono(start, ops, i + 1, ops.len());
//@     }
//@ }

//@ // The shipped steps are the operations of the run: take_step's due is op_due of a take, skip_step's of a skip (from their
//@ // ensures above), so lemma_once_and_complete is about what the cursor in lib.rs computes with them.
//@ proof fn lemma_shipped_steps_are_ops(expected: u64, seq: u64, to: u64, took: (u64, Taken), skipped: u64)
//@     requires
//@         took == step(expected, seq),
//@         skipped >= expected && skipped >= to && (skipped == expected || skipped == to),
//@     ensures
//@         took.0 == op_due(expected, (seq, true)),
//@         forall|n: u64| reports(took.1, seq, n) <==> #[trigger] op_reports(expected, (seq, true), n),
//@         skipped == op_due(expected, (to, false)),
//@ {
//@ }
