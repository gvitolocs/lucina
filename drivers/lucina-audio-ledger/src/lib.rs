// SPDX-License-Identifier: MPL-2.0

//! Juno's audio bookkeeping that must not miscount (`docs/research/juno-audio.md`, section 26).
//!
//! # Periods handed to the DSP
//!
//! Playback and capture share a buffer of `periods` equal slots with the DSP. The original is Linux's `q6asm`: `q6asm_write_async`
//! and `q6asm_read` hand out slot `port->dsp_buf` and step it round (`q6asm.c`), and each `ASM_DATA_EVENT_WRITE_DONE_V2` /
//! `READ_DONE_V2` gives one back, oldest first, with its slot as the token. A [`Ledger`] is that, kept so that it cannot
//! miscount: the oldest slot still out, how many are out, and which of them hold samples (a bit per slot). [`queue`] hands out
//! [`next_slot`], which is never a slot still out, and [`done`] takes back only the oldest slot; anything else (a slot that is
//! not out, a second answer for one) is refused and changes nothing, where the kernel's `saturating_sub` used to hide it.
//!
//! # The microphone queue
//!
//! [`mic_push`]: what a push keeps and what it drops when a program reads slower than the microphone delivers, so that the
//! `mic_dropped` count is exactly the bytes lost.
//!
//! # Undoing power steps
//!
//! [`undo_step`]: the order to undo the steps a power-up took, each exactly once, also when it stopped half way. Linux undoes the
//! VA macro's votes and clocks in reverse (`va_macro_remove`, devm); Windows' `\_SB.RTK0` D3 undoes its GPIO and its regulator in
//! the order D0 took them ([`Undo::AsTaken`]).
//!
//! Verus proves this very file (`verification/juno-audio-ledger/rules.rs`, which adds the FIFO and the undo proofs): what each
//! function promises is in its `ensures`.
#![no_std]
#![allow(unexpected_cfgs)]

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

/// The most slots a ledger keeps: one bit each in [`Ledger::data`].
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const MAX_PERIODS: u32 = 32;

/// The periods out with the DSP: slots `oldest`, `oldest + 1`, ... (round the ring), `count` of them. Bit `s` of `data` is set
/// when slot `s` is out and holds samples, so a stream has played everything once `data` is 0.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Ledger {
    pub oldest: u32,
    pub count: u32,
    pub data: u32,
}

#[cfg(verus_keep_ghost)]
verus! {

/// Bit `s` of `x`.
pub open spec fn bit(x: u32, s: u32) -> bool {
    ((x >> s) & 1u32) == 1u32
}

/// How far slot `s` is past `oldest`, round a ring of `p` slots.
pub open spec fn dist(oldest: u32, s: u32, p: u32) -> int {
    if s >= oldest { s - oldest } else { s + p - oldest }
}

/// The slot `i` places after `oldest`.
pub open spec fn slot_at(oldest: u32, i: int, p: u32) -> int {
    if oldest + i < p { oldest + i } else { oldest + i - p }
}

/// The slot after the last one out: where the next period goes.
pub open spec fn nxt(l: Ledger, p: u32) -> u32 {
    if l.oldest + l.count >= p { (l.oldest + l.count - p) as u32 } else { (l.oldest + l.count) as u32 }
}

/// Slot `s` is out with the DSP.
pub open spec fn out(l: Ledger, s: u32, p: u32) -> bool {
    s < p && dist(l.oldest, s, p) < l.count as int
}

/// A ledger for a ring of `p` slots: in range, and only slots that are out have their data bit set.
pub open spec fn ok(l: Ledger, p: u32) -> bool {
    &&& 1 <= p <= MAX_PERIODS
    &&& l.oldest < p
    &&& l.count <= p
    &&& forall|s: u32| s < 32 && #[trigger] bit(l.data, s) ==> out(l, s, p)
}

pub proof fn lemma_set(x: u32, s: u32, t: u32)
    requires
        s < 32,
        t < 32,
    ensures
        bit(x | (1u32 << t), s) == (s == t || bit(x, s)),
        bit(x & !(1u32 << t), s) == (s != t && bit(x, s)),
{
    assert(((x | (1u32 << t)) >> s) & 1u32 == 1u32 <==> (s == t || (x >> s) & 1u32 == 1u32)) by (bit_vector)
        requires s < 32u32, t < 32u32;
    assert(((x & !(1u32 << t)) >> s) & 1u32 == 1u32 <==> (s != t && (x >> s) & 1u32 == 1u32)) by (bit_vector)
        requires s < 32u32, t < 32u32;
}

pub proof fn lemma_zero(s: u32)
    requires
        s < 32,
    ensures
        !bit(0u32, s),
{
    assert((0u32 >> s) & 1u32 != 1u32) by (bit_vector);
}

} // verus!

/// An empty ledger: nothing out.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires
        1 <= periods <= MAX_PERIODS,
    ensures
        ok(r, periods),
        r.count == 0,
        r.data == 0,
))]
pub fn empty(periods: u32) -> Ledger {
    let _ = periods; // read by the proof only
    #[cfg(verus_keep_ghost)]
    proof! { assert forall|s: u32| s < 32 && #[trigger] bit(0u32, s) implies false by { lemma_zero(s); } }
    Ledger { oldest: 0, count: 0, data: 0 }
}

/// The slot the next period goes in: the one after the last that is out. While fewer than `periods` are out it is not out.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires
        ok(l, periods),
        l.count < periods,
    ensures
        r == nxt(l, periods),
        r < periods,
        r as int == slot_at(l.oldest, l.count as int, periods),
        dist(l.oldest, r, periods) == l.count as int,
        !out(l, r, periods),
))]
pub fn next_slot(l: Ledger, periods: u32) -> u32 {
    let s = l.oldest + l.count;
    if s >= periods { s - periods } else { s }
}

/// Whether one more period may go out, with at most `limit` out at once.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == (l.count < limit),
))]
pub fn may_queue(l: Ledger, limit: u32) -> bool {
    l.count < limit
}

/// Hand out [`next_slot`], holding samples or not. The slot was not out; now it is, and only its data bit changed.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires
        ok(l, periods),
        l.count < periods,
    ensures
        ok(r, periods),
        r.oldest == l.oldest,
        r.count == l.count + 1,
        out(r, nxt(l, periods), periods),
        bit(r.data, nxt(l, periods)) == has_data,
        forall|s: u32| s < 32 && s != nxt(l, periods) ==> #[trigger] bit(r.data, s) == bit(l.data, s),
        forall|s: u32| s < periods && s != nxt(l, periods) ==> (#[trigger] out(r, s, periods) == out(l, s, periods)),
))]
pub fn queue(l: Ledger, periods: u32, has_data: bool) -> Ledger {
    let slot = next_slot(l, periods);
    let data = if has_data { l.data | (1u32 << slot) } else { l.data & !(1u32 << slot) };
    #[cfg(verus_keep_ghost)]
    proof! {
        assert forall|s: u32| s < 32 implies #[trigger] bit(data, s) == (if s == slot { has_data } else { bit(l.data, s) }) by {
            lemma_set(l.data, s, slot);
        }
    }
    Ledger { oldest: l.oldest, count: l.count + 1, data }
}

/// The DSP gave slot `slot` back. Taken only when it is the oldest out; then it is no longer out, its data bit is cleared, and
/// the answer says whether it held samples. Anything else is refused (`None`) and the ledger stays as it was.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires
        ok(l, periods),
    ensures
        r.is_none() == (l.count == 0 || slot != l.oldest),
        r matches Some((n, had)) ==> {
            &&& ok(n, periods)
            &&& n.count == l.count - 1
            &&& n.oldest as int == slot_at(l.oldest, 1, periods)
            &&& had == bit(l.data, slot)
            &&& !out(n, slot, periods)
            &&& !bit(n.data, slot)
            &&& forall|s: u32| s < 32 && s != slot ==> #[trigger] bit(n.data, s) == bit(l.data, s)
            &&& forall|s: u32| s < periods && s != slot ==> (#[trigger] out(n, s, periods) == out(l, s, periods))
        },
))]
pub fn done(l: Ledger, periods: u32, slot: u32) -> Option<(Ledger, bool)> {
    if l.count == 0 || slot != l.oldest {
        return None;
    }
    let had = (l.data >> slot) & 1 == 1;
    let data = l.data & !(1u32 << slot);
    let oldest = if l.oldest + 1 >= periods { 0 } else { l.oldest + 1 };
    #[cfg(verus_keep_ghost)]
    proof! {
        assert forall|s: u32| s < 32 implies #[trigger] bit(data, s) == (s != slot && bit(l.data, s)) by {
            lemma_set(l.data, s, slot);
        }
    }
    Some((Ledger { oldest, count: l.count - 1, data }, had))
}

/// Whether a period that holds samples is still out: once this is false, everything queued has gone through.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires
        ok(l, periods),
    ensures
        r == (l.data != 0),
        !r ==> forall|s: u32| s < 32 ==> !#[trigger] bit(l.data, s),
))]
pub fn data_out(l: Ledger, periods: u32) -> bool {
    let _ = periods; // read by the proof only
    #[cfg(verus_keep_ghost)]
    proof! {
        if l.data == 0 {
            assert forall|s: u32| s < 32 implies !#[trigger] bit(l.data, s) by { lemma_zero(s); }
        }
    }
    l.data != 0
}

/// Whether a period that went out empty is an underrun: samples had started, the program has not finished, and it is not the
/// test tone (which is never late). Counted only for a period that really went out.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == (bytes == 0 && had_data && !tone && !source_finished),
))]
pub fn is_underrun(bytes: u32, had_data: bool, tone: bool, source_finished: bool) -> bool {
    bytes == 0 && had_data && !tone && !source_finished
}

/// A push of `new` bytes onto a microphone queue holding `held` of at most `cap`: how many it holds after (the newest) and how
/// many of the oldest it drops. Nothing is lost that fits, and what is dropped is exactly what does not.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires
        held <= cap,
        held + new <= usize::MAX,
    ensures
        r.0 + r.1 == held + new,
        r.0 <= cap,
        r.1 as int == if held + new > cap { held + new - cap } else { 0 },
        r.1 <= new,
))]
pub fn mic_push(held: usize, new: usize, cap: usize) -> (usize, usize) {
    let total = held + new;
    if total > cap { (cap, total - cap) } else { (total, 0) }
}

/// The order power steps are undone in.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Undo {
    /// Last taken, first undone (Linux's devm release of the VA macro's votes and clocks).
    Reverse,
    /// In the order taken (Windows' `\_SB.RTK0` D3: GPIO 58 low, then the LDO15_A vote off).
    AsTaken,
}

/// The `i`-th step to undo, of the first `taken` steps of a power-up (those that were done, also when it stopped half way).
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires
        i < taken,
    ensures
        r < taken,
        order == Undo::Reverse ==> r == taken - 1 - i,
        order == Undo::AsTaken ==> r == i,
))]
pub fn undo_step(order: Undo, taken: u32, i: u32) -> u32 {
    match order {
        Undo::Reverse => taken - 1 - i,
        Undo::AsTaken => i,
    }
}

#[cfg(test)]
mod tests;
