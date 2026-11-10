// SPDX-License-Identifier: MPL-2.0

//! When Juno's audio thread runs (`docs/research/juno-audio.md`, section 27).
//!
//! The thread used to look at everything every 5 ms, and every 1 ms with a stream open: 200 to 1000 wakes a second, also with
//! the laptop asleep. Linux and Windows do not poll here. Their drivers run from the interrupts that the DSP raises:
//! `qcom_glink_smem_intr` for GLINK (each APR answer and each `ASM_DATA_EVENT_WRITE_DONE_V2` / `READ_DONE_V2` comes this way),
//! `qcom_smp2p_intr` for the DSP's state bits, and `q6v5_wdog_interrupt`. The lines are not guessed. Juno reads them from the
//! device tree, and Windows' ACPI tables give the same ones: GLINK edge `interrupts` SPI 156 (`\_SB.GLNK` 0xBC), `smp2p-lpass`
//! SPI 158 (`\_SB.GLNK` 0xBE), and the ADSP's `wdog` SPI 162 (`\_SB.ADSP` 0xC2). All three are edge-triggered and active high.
//!
//! # Raise, drain, sleep
//!
//! Each handler [`raise`]s its own bit in one pending word and wakes the thread. The device files raise [`FILES`] when they
//! change what the thread looks at. A pass begins with [`drain`]: it takes the word and leaves it clear *before* any source is
//! read. So whatever comes during the pass is raised again, and the pass after it sees it. After the pass the thread waits only
//! when [`may_sleep`] allows it: nothing has been raised since the drain, and the pass left nothing behind (`Glink::rx_pending`).
//! Its wait condition is that same check, made again by the wait queue after the waiter is queued. `verification/juno-audio-wake`
//! proves that no event is lost over every interleaving of interrupts with the pass, and that the other order (read, then
//! clear) loses one.
//!
//! # Deadlines
//!
//! Some things still happen at a time: an answer the DSP owes ([`first_after`] gives the first millisecond at which
//! `now - sent_at > timeout` holds), the next question while the framework starts up ([`first_at`]), and a line in the log
//! while a stream is open ([`beat_due`]). [`next_due`] is the earliest of them, and [`sleep_for`] turns it into a wait. Once
//! the framework is ready, with no stream or microphone open or being set up, and no GLINK expiry pending, there is no
//! deadline. The thread then waits with no timer at all.
//!
//! Verus proves this very file (`verification/juno-audio-wake/rules.rs`). What each function promises is in its `ensures`.
#![no_std]
#![allow(unexpected_cfgs)]

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

/// GLINK from the DSP: the `glink-edge` node's `interrupts`, SPI 156, edge rising (Windows `\_SB.GLNK` 0xBC).
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const GLINK: u32 = 1;
/// The DSP's SMP2P state bits: `smp2p-lpass` `interrupts`, SPI 158, edge rising (Windows `\_SB.GLNK` 0xBE).
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const SMP2P: u32 = 2;
/// The DSP's watchdog: the first `interrupts-extended` entry of `qcom,sc7180-adsp-pas`, SPI 162 (Windows `\_SB.ADSP` 0xC2).
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const WATCHDOG: u32 = 4;
/// A device file changed what the thread looks at: a start, a close, the microphone opened or closed, a restart asked for.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const FILES: u32 = 8;

#[cfg(verus_keep_ghost)]
verus! {

/// One of the sources that wake the thread.
pub open spec fn is_source(s: u32) -> bool {
    s == GLINK || s == SMP2P || s == WATCHDOG || s == FILES
}

/// The earliest of two deadlines, `None` being none.
pub open spec fn min_due(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (None, None) => None,
        (Some(x), None) => Some(x),
        (None, Some(y)) => Some(y),
        (Some(x), Some(y)) => Some(if x <= y { x } else { y }),
    }
}

/// Milliseconds from `since` to `now`.
pub open spec fn elapsed(now: u64, since: u64) -> int {
    now - since
}

/// [`first_after`], as a formula.
pub open spec fn after_of(since: u64, limit: u64) -> u64 {
    if limit >= u64::MAX - since { u64::MAX } else { (since + limit + 1) as u64 }
}

/// [`first_at`], as a formula.
pub open spec fn at_of(since: u64, limit: u64) -> u64 {
    if limit > u64::MAX - since { u64::MAX } else { (since + limit) as u64 }
}

/// [`next_due`], as a formula.
pub open spec fn due_of(d: Due) -> Option<u64> {
    let r = min_due(min_due(min_due(d.stream, d.mic), d.link), d.beat_at);
    if d.ready { r } else { min_due(min_due(r, d.ask_at), Some(d.give_up_at)) }
}

proof fn lemma_or(p: u32, s: u32)
    ensures
        (p | s) & s == s,
        (p | s) & p == p,
        s != 0 ==> (p | s) != 0,
{
    assert((p | s) & s == s && (p | s) & p == p && (s != 0 ==> (p | s) != 0)) by (bit_vector);
}

} // verus!

/// What an interrupt handler or a device file leaves in the pending word: its own bit, added to what is there. Nothing raised
/// before is lost, and the word is not clear afterwards.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires
        is_source(source),
    ensures
        r == pending | source,
        r & source == source,
        r & pending == pending,
        r != 0,
))]
pub fn raise(pending: u32, source: u32) -> u32 {
    #[cfg(verus_keep_ghost)]
    proof! {
        lemma_or(pending, source);
    }
    pending | source
}

/// The start of a pass: what is taken (everything raised so far) and what is left in the word (nothing). The kernel stores
/// the second before it reads any source.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r.0 == pending,
        r.1 == 0,
))]
pub fn drain(pending: u32) -> (u32, u32) {
    (pending, 0)
}

/// After a pass: the thread may wait only when nothing has been raised since the drain and the pass left nothing behind.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == (pending == 0 && !left_behind),
))]
pub fn may_sleep(pending: u32, left_behind: bool) -> bool {
    pending == 0 && !left_behind
}

/// The first millisecond at which `now - since > limit` holds (an answer timeout: `now_ms() - sent_at > answer_timeout_ms()`).
/// `u64::MAX` when that is past the clock's end.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == after_of(since, limit),
        r > since || r == u64::MAX,
        limit < u64::MAX - since ==> r == since + limit + 1,
        limit < u64::MAX - since ==> forall|now: u64| since <= now ==> (now >= r <==> #[trigger] elapsed(now, since) > limit as int),
))]
pub fn first_after(since: u64, limit: u64) -> u64 {
    if limit >= u64::MAX - since {
        u64::MAX
    } else {
        since + limit + 1
    }
}

/// The first millisecond at which `now - since >= limit` holds (asking again: `now - at >= ASK_TIMEOUT_MS`). `u64::MAX` when
/// that is past the clock's end.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == at_of(since, limit),
        r >= since,
        limit <= u64::MAX - since ==> r == since + limit,
        limit <= u64::MAX - since ==> forall|now: u64| since <= now ==> (now >= r <==> #[trigger] elapsed(now, since) >= limit as int),
))]
pub fn first_at(since: u64, limit: u64) -> u64 {
    if limit > u64::MAX - since {
        u64::MAX
    } else {
        since + limit
    }
}

/// The answer deadline of a stream: none unless it waits for the DSP.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        !waiting ==> r.is_none(),
        waiting ==> r == Some(after_of(sent_at, limit)),
))]
pub fn answer_due(waiting: bool, sent_at: u64, limit: u64) -> Option<u64> {
    if waiting {
        Some(first_after(sent_at, limit))
    } else {
        None
    }
}

/// The next line in the log: only while a stream or the microphone is open, `every` after the last one.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        !open ==> r.is_none(),
        open ==> r == Some(at_of(last, every)),
))]
pub fn beat_due(open: bool, last: u64, every: u64) -> Option<u64> {
    if open {
        Some(first_at(last, every))
    } else {
        None
    }
}

/// The earlier of two deadlines.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == min_due(a, b),
))]
pub fn earliest(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(x), Some(y)) => {
            if y < x {
                Some(y)
            } else {
                Some(x)
            }
        }
        (Some(_), None) => a,
        (None, _) => b,
    }
}

/// What the thread has to do at a time, for one wait.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Due {
    /// The audio framework said it is ready. Before that, `ask_at` and `give_up_at` count.
    pub ready: bool,
    /// When to ask for its state again, once a question is out.
    pub ask_at: Option<u64>,
    /// When to give up on it and start the DSP again.
    pub give_up_at: u64,
    /// The playback's answer deadline ([`answer_due`]).
    pub stream: Option<u64>,
    /// The microphone's answer deadline ([`answer_due`]).
    pub mic: Option<u64>,
    /// GLINK's earliest expiry (`Glink::next_deadline`).
    pub link: Option<u64>,
    /// The next line in the log ([`beat_due`]).
    pub beat_at: Option<u64>,
}

/// The earliest deadline there is, or none.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == due_of(d),
))]
pub fn next_due(d: Due) -> Option<u64> {
    let r = earliest(earliest(earliest(d.stream, d.mic), d.link), d.beat_at);
    if d.ready {
        r
    } else {
        earliest(earliest(r, d.ask_at), Some(d.give_up_at))
    }
}

/// How the thread waits.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Sleep {
    /// Until something is raised: no timer.
    Forever,
    /// Until something is raised, or this many milliseconds.
    For(u64),
    /// Not at all: something is due now.
    No,
}

/// A wait that ends at `deadline` (or on a raise), or never ends by itself when there is none.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        deadline.is_none() ==> r == Sleep::Forever,
        deadline.is_some() && deadline.unwrap() > now ==> r == Sleep::For((deadline.unwrap() - now) as u64),
        deadline.is_some() && deadline.unwrap() <= now ==> r == Sleep::No,
))]
pub fn sleep_for(deadline: Option<u64>, now: u64) -> Sleep {
    match deadline {
        None => Sleep::Forever,
        Some(d) if d > now => Sleep::For(d - now),
        Some(_) => Sleep::No,
    }
}

#[cfg(test)]
mod tests;
