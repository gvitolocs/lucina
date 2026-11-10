// Verus proof of the SHIPPED code, not a model of it (project rule D-011): this file includes
// drivers/lucina-audio-wake/src/lib.rs by #[path], so Verus checks the functions as the kernel compiles them; what each promises
// is in its `ensures`. On top of those, this file proves:
// - the sources are four different bits, none of them 0;
// - no event is lost: the audio thread's loop (drain, read the sources, decide, sleep) runs against interrupts that may come at
//   any point. The steps below call the shipped `raise`, `drain` and `may_sleep`, and each keeps an invariant that says a
//   sleeping thread with news it has not read has a pending word that is not 0, which is the wait queue's wake condition;
// - the other order (read the sources, then clear the word) loses an event;
// - deadlines: with the framework ready and nothing open there is none, so the wait has no timer; the earliest deadline is never
//   later than any that counts and is always one of them; a timed wait ends exactly at the deadline.
#![feature(proc_macro_hygiene)]
#[path = "../../drivers/lucina-audio-wake/src/lib.rs"]
pub mod wake;

use vstd::prelude::*;
use wake::*;

verus! {

proof fn lemma_sources_apart()
    ensures
        GLINK != 0 && SMP2P != 0 && WATCHDOG != 0 && FILES != 0,
        GLINK & SMP2P == 0 && GLINK & WATCHDOG == 0 && GLINK & FILES == 0,
        SMP2P & WATCHDOG == 0 && SMP2P & FILES == 0 && WATCHDOG & FILES == 0,
{
    assert(1u32 & 2u32 == 0 && 1u32 & 4u32 == 0 && 1u32 & 8u32 == 0) by (bit_vector);
    assert(2u32 & 4u32 == 0 && 2u32 & 8u32 == 0 && 4u32 & 8u32 == 0) by (bit_vector);
}

/// Where the audio thread is in its loop.
#[derive(PartialEq, Eq)]
pub enum Phase {
    /// The pending word was just taken and cleared; the sources are about to be read.
    Drained,
    /// The sources were read; the thread is about to decide whether to sleep.
    Read,
    /// Waiting on the queue, with `may_sleep` as its condition.
    Asleep,
}

/// The pending word, whether the hardware has news the thread has not read (`unseen`), what the last read left behind (GLINK's
/// per-poll limit), and the thread's phase.
pub struct M {
    pub pending: u32,
    pub unseen: bool,
    pub left: bool,
    pub phase: Phase,
}

pub open spec fn inv(m: M) -> bool {
    match m.phase {
        Phase::Drained => true,
        Phase::Read => m.unseen ==> (m.pending != 0 || m.left),
        Phase::Asleep => m.unseen ==> m.pending != 0,
    }
}

/// An interrupt (or a device file), at any point of the loop: the hardware has news, and its bit is raised.
fn irq(m: M, source: u32) -> (r: M)
    requires
        inv(m),
        is_source(source),
    ensures
        inv(r),
        r.unseen,
        r.pending != 0,
        r.phase == m.phase,
{
    M { pending: raise(m.pending, source), unseen: true, left: m.left, phase: m.phase }
}

/// The start of a pass: the word is taken and cleared before any source is read.
fn begin(m: M) -> (r: M)
    requires
        inv(m),
    ensures
        inv(r),
        r.phase == Phase::Drained,
        r.pending == 0,
{
    let (_taken, left) = drain(m.pending);
    M { pending: left, unseen: m.unseen, left: m.left, phase: Phase::Drained }
}

/// Reading every source: all news is read, but for what the read leaves behind.
fn read(m: M, left_behind: bool) -> (r: M)
    requires
        inv(m),
        m.phase == Phase::Drained,
    ensures
        inv(r),
        r.phase == Phase::Read,
{
    M { pending: m.pending, unseen: left_behind, left: left_behind, phase: Phase::Read }
}

/// After the read: sleep, or go round again.
fn decide(m: M) -> (r: M)
    requires
        inv(m),
        m.phase == Phase::Read,
    ensures
        inv(r),
        r.phase == Phase::Asleep ==> !r.unseen && r.pending == 0,
        r.phase != Phase::Asleep ==> r.phase == Phase::Drained,
{
    if may_sleep(m.pending, m.left) {
        M { pending: m.pending, unseen: m.unseen, left: m.left, phase: Phase::Asleep }
    } else {
        begin(m)
    }
}

/// What the wait queue promises: a sleeping thread whose condition holds is woken; and a timed wait may end anyway.
fn wake_up(m: M) -> (r: M)
    requires
        inv(m),
        m.phase == Phase::Asleep,
    ensures
        inv(r),
        r.phase == Phase::Drained,
{
    begin(m)
}

/// No event is lost: a thread that sleeps with news it has not read has its wake condition true, so it is woken.
proof fn lemma_no_lost_event(m: M)
    requires
        inv(m),
        m.phase == Phase::Asleep,
        m.unseen,
    ensures
        !may_sleep_spec(m.pending, m.left),
{
}

pub open spec fn may_sleep_spec(pending: u32, left_behind: bool) -> bool {
    pending == 0 && !left_behind
}

/// The other order, read first and clear after, loses an event: an interrupt between the read and the clear leaves the thread
/// asleep with news and a clear word, so nothing wakes it.
proof fn lemma_read_then_clear_loses_one()
    ensures
        ({
            let after_read = M { pending: 0, unseen: false, left: false, phase: Phase::Read };
            let after_irq = M { pending: after_read.pending | GLINK, unseen: true, left: false, phase: Phase::Read };
            let after_clear = M { pending: 0, unseen: after_irq.unseen, left: false, phase: Phase::Asleep };
            after_irq.pending != 0 && may_sleep_spec(after_clear.pending, after_clear.left) && !inv(after_clear)
        }),
{
    assert(0u32 | 1u32 == 1u32) by (bit_vector);
}

spec fn not_after(r: Option<u64>, x: Option<u64>) -> bool {
    x.is_some() ==> (r.is_some() && r.unwrap() <= x.unwrap())
}

spec fn is_one_of(r: Option<u64>, d: Due) -> bool {
    r.is_some() ==> (r == d.stream || r == d.mic || r == d.link || r == d.beat_at || (!d.ready && (r == d.ask_at || r == Some(
        d.give_up_at,
    ))))
}

proof fn lemma_min(a: Option<u64>, b: Option<u64>)
    ensures
        not_after(min_due(a, b), a),
        not_after(min_due(a, b), b),
        min_due(a, b).is_some() ==> (min_due(a, b) == a || min_due(a, b) == b),
        min_due(a, b).is_none() <==> (a.is_none() && b.is_none()),
{
}

proof fn lemma_not_after_min(r: Option<u64>, a: Option<u64>, b: Option<u64>, x: Option<u64>)
    requires
        not_after(a, x),
    ensures
        not_after(min_due(a, b), x),
{
}

/// The earliest deadline is never later than any that counts: none is missed.
proof fn lemma_due_never_late(d: Due)
    ensures
        not_after(due_of(d), d.stream),
        not_after(due_of(d), d.mic),
        not_after(due_of(d), d.link),
        not_after(due_of(d), d.beat_at),
        !d.ready ==> not_after(due_of(d), d.ask_at),
        !d.ready ==> not_after(due_of(d), Some(d.give_up_at)),
{
    let a = min_due(d.stream, d.mic);
    let b = min_due(a, d.link);
    let c = min_due(b, d.beat_at);
    lemma_min(d.stream, d.mic);
    lemma_min(a, d.link);
    lemma_min(b, d.beat_at);
    lemma_not_after_min(a, a, d.link, d.stream);
    lemma_not_after_min(a, a, d.link, d.mic);
    lemma_not_after_min(b, b, d.beat_at, d.stream);
    lemma_not_after_min(b, b, d.beat_at, d.mic);
    lemma_not_after_min(b, b, d.beat_at, d.link);
    if !d.ready {
        let e = min_due(c, d.ask_at);
        lemma_min(c, d.ask_at);
        lemma_min(e, Some(d.give_up_at));
        lemma_not_after_min(e, c, d.ask_at, d.stream);
        lemma_not_after_min(e, c, d.ask_at, d.mic);
        lemma_not_after_min(e, c, d.ask_at, d.link);
        lemma_not_after_min(e, c, d.ask_at, d.beat_at);
        lemma_not_after_min(e, e, Some(d.give_up_at), d.stream);
        lemma_not_after_min(e, e, Some(d.give_up_at), d.mic);
        lemma_not_after_min(e, e, Some(d.give_up_at), d.link);
        lemma_not_after_min(e, e, Some(d.give_up_at), d.beat_at);
        lemma_not_after_min(e, e, Some(d.give_up_at), d.ask_at);
    }
}

/// The earliest deadline is always one of them: the thread never wakes for nothing.
proof fn lemma_due_is_one(d: Due)
    ensures
        is_one_of(due_of(d), d),
{
    let a = min_due(d.stream, d.mic);
    let b = min_due(a, d.link);
    let c = min_due(b, d.beat_at);
    lemma_min(d.stream, d.mic);
    lemma_min(a, d.link);
    lemma_min(b, d.beat_at);
    if !d.ready {
        lemma_min(c, d.ask_at);
        lemma_min(min_due(c, d.ask_at), Some(d.give_up_at));
    }
}

/// Ready, nothing open or being set up, no GLINK expiry: no deadline.
proof fn lemma_idle_no_deadline(d: Due)
    requires
        d.ready,
        d.stream.is_none(),
        d.mic.is_none(),
        d.link.is_none(),
        d.beat_at.is_none(),
    ensures
        due_of(d).is_none(),
{
}

/// The thread with the framework ready and no stream or microphone open, as the kernel builds its `Due`, waits with no timer.
fn idle_waits_with_no_timer(ask_at: Option<u64>, give_up_at: u64, sent_at: u64, mic_sent_at: u64, limit: u64, last: u64, every: u64, now: u64)
    ensures
        ({
            let d = Due {
                ready: true,
                ask_at,
                give_up_at,
                stream: None,
                mic: None,
                link: None,
                beat_at: None,
            };
            due_of(d).is_none()
        }),
{
    let d = Due {
        ready: true,
        ask_at,
        give_up_at,
        stream: answer_due(false, sent_at, limit),
        mic: answer_due(false, mic_sent_at, limit),
        link: None,
        beat_at: beat_due(false, last, every),
    };
    let due = next_due(d);
    assert(due.is_none());
    let s = sleep_for(due, now);
    assert(s == Sleep::Forever);
}

/// A timed wait ends exactly at the deadline.
fn timed_wait_ends_at_deadline(d: u64, now: u64)
    requires
        d > now,
{
    match sleep_for(Some(d), now) {
        Sleep::For(n) => assert(now + n == d),
        _ => assert(false),
    }
}

/// An answer the DSP owes times out at the first millisecond the kernel's check (`now - sent_at > limit`) holds, not before.
fn answer_times_out_exactly(sent_at: u64, limit: u64, now: u64)
    requires
        limit < u64::MAX - sent_at,
        sent_at <= now,
{
    let d = answer_due(true, sent_at, limit).unwrap();
    assert(elapsed(now, sent_at) == now - sent_at);
    assert((now >= d) == (now - sent_at > limit));
}

fn main() {
}

} // verus!
