// The integer rules under the voice stream (docs/voice-streaming-survey.md, LucinaVoice protocol section 10): the 48 kHz to
// 16 kHz decimator's windows and filter, the 24 kHz mono to 48 kHz stereo expansion, the client's turn phases, and the table
// of voice notes still waiting for their answer. Verus checks this very file: `//@` lines hold the proof, see
// scripts/verify-shipped.sh (D-011); verification/voice-stream/mutants.sh plants bugs that Verus must refuse.
// `dsp.rs` and `session.rs` call these functions and keep no copy of the rules.

//@ pub open spec fn olen(blen: int, phase: int) -> int { if blen < phase + 31 { 0 } else { (blen - phase - 31) / 3 + 1 } }

/// Taps of the low-pass filter (a Hann-windowed sinc, 7 kHz corner at 48 kHz, gain 1 at DC in Q15).
pub const TAPS: usize = 31;
/// Input samples a window needs from before the newest one.
pub const HIST: usize = 30;
/// 48 kHz to 16 kHz.
pub const FACTOR: usize = 3;

/// Filter tap `k`, Q15: all 31 sum to 32768.
pub fn coef(k: usize) -> i32 //@ret
//@ requires k < 31,
//@ ensures -32768 <= ret <= 32767,
{
    match k {
        0 => 0,
        1 => 2,
        2 => -21,
        3 => -83,
        4 => -95,
        5 => 67,
        6 => 370,
        7 => 505,
        8 => 107,
        9 => -804,
        10 => -1550,
        11 => -1087,
        12 => 1203,
        13 => 4816,
        14 => 8179,
        15 => 9550,
        16 => 8179,
        17 => 4816,
        18 => 1203,
        19 => -1087,
        20 => -1550,
        21 => -804,
        22 => 107,
        23 => 505,
        24 => 370,
        25 => 67,
        26 => -95,
        27 => -83,
        28 => -21,
        29 => 2,
        30 => 0,
        _ => 0,
    }
}

/// A sample kept in 16 bits.
pub fn clamp16(v: i64) -> i16 //@ret
//@ ensures -32768 <= ret <= 32767, (-32768 <= v <= 32767) ==> ret as int == v, v < -32768 ==> ret == -32768, v > 32767 ==> ret == 32767,
{
    if v < -32768 {
        -32768
    } else if v > 32767 {
        32767
    } else {
        v as i16
    }
}

/// How many 16 kHz samples a buffer of `blen` samples (the 30 carried ones, then the new ones) gives when the next window
/// starts at `phase`: windows start at `phase`, `phase + 3`, ... and each needs 31 samples.
pub fn out_len(blen: usize, phase: usize) -> usize //@ret
//@ requires HIST <= blen <= 268435456, phase < 3,
//@ ensures
//@     ret as int == olen(blen as int, phase as int),
//@     ret == 0 ==> blen < phase + 31,
//@     ret > 0 ==> phase + 3 * (ret - 1) + 31 <= blen,
//@     phase + 3 * ret + 31 > blen,
{
    if blen < phase + TAPS {
        0
    } else {
        (blen - phase - TAPS) / FACTOR + 1
    }
}

/// The filter at the window that starts at `s`: the sum of 31 taps times 31 samples, in Q15.
pub fn filter_at(buf: &[i16], s: usize) -> i16 //@ret
//@ requires s + 31 <= buf.len(),
{
    let mut acc: i64 = 0;
    let mut k: usize = 0;
    while k < TAPS
    //@ invariant k <= 31, s + 31 <= buf.len(), -(k as i64) * 1073741824 <= acc <= (k as i64) * 1073741824,
    //@ decreases 31 - k,
    {
        let c = coef(k) as i64;
        let x = buf[s + k] as i64;
        //@ assert(-1073741824 <= c * x <= 1073741824) by (nonlinear_arith) requires -32768 <= c <= 32767, -32768 <= x <= 32767;
        acc = acc + c * x;
        k += 1;
    }
    clamp16(acc / 32768)
}

/// Decimates `buf`, which holds the 30 samples carried from the last call and then this call's new ones, by 3. The windows
/// start at `phase`, `phase + 3`, ...; the output gets one sample per window, and the result is the phase for the next call,
/// whose buffer begins with the last 30 samples of this one. Every read is inside `buf`, and the phase stays under 3.
pub fn decimate(buf: &[i16], phase: usize, out: &mut Vec<i16>) -> usize //@ret
//@ requires HIST <= buf.len() <= 268435456, phase < 3, old(out)@.len() < 1000000000,
//@ ensures
//@     ret < 3,
//@     final(out)@.len() == old(out)@.len() + olen(buf.len() as int, phase as int),
//@     ret as int == phase as int + 3 * olen(buf.len() as int, phase as int) - (buf.len() as int - 30),
{
    let n = out_len(buf.len(), phase);
    let mut j: usize = 0;
    while j < n
    //@ invariant
    //@     j <= n, n as int == olen(buf.len() as int, phase as int), 30 <= buf.len() <= 268435456, phase < 3,
    //@     n > 0 ==> phase + 3 * (n - 1) + 31 <= buf.len(),
    //@     out@.len() == old(out)@.len() + j, old(out)@.len() < 1000000000, n <= 268435456,
    //@ decreases n - j,
    {
        let s = phase + FACTOR * j;
        out.push(filter_at(buf, s));
        j += 1;
    }
    phase + FACTOR * n - (buf.len() - HIST)
}

/// 24 kHz mono to 48 kHz stereo: each input sample becomes four output samples, the midpoint with the one before it and then
/// the sample itself, each as left and right. `prev` is the last sample of the call before; the result is the last sample of
/// this one (or `prev` when `inp` is empty). The output grows by exactly `4 * inp.len()`.
pub fn expand(inp: &[i16], prev: i16, out: &mut Vec<i16>) -> i16 //@ret
//@ requires inp.len() <= 268435456, old(out)@.len() < 1000000000,
//@ ensures
//@     final(out)@.len() == old(out)@.len() + 4 * inp.len(),
//@     ret == if inp.len() == 0 { prev } else { inp[inp.len() - 1] },
{
    let mut last = prev;
    let mut i: usize = 0;
    while i < inp.len()
    //@ invariant
    //@     i <= inp.len(), inp.len() <= 268435456, out@.len() == old(out)@.len() + 4 * i, old(out)@.len() < 1000000000,
    //@     last == if i == 0 { prev } else { inp[i - 1] },
    //@ decreases inp.len() - i,
    {
        let x = inp[i];
        let sum = last as i32 + x as i32;
        //@ assert(-32768 <= sum / 2 <= 32767);
        let mid = (sum / 2) as i16;
        out.push(mid);
        out.push(mid);
        out.push(x);
        out.push(x);
        last = x;
        i += 1;
    }
    last
}

/// Whole 16-bit samples in `n` bytes, and the bytes left over (a sample split between two frames).
pub fn split_samples(n: usize) -> (usize, usize) //@ret
//@ ensures ret.0 * 2 + ret.1 == n, ret.1 < 2,
{
    (n / 2, n % 2)
}

pub const IDLE: u8 = 0;
pub const STARTING: u8 = 1;
pub const STREAMING: u8 = 2;
pub const WAITING: u8 = 3;

pub const EV_PRESS: u8 = 0;
pub const EV_LISTENING: u8 = 1;
pub const EV_END: u8 = 2;
pub const EV_ENDPOINT: u8 = 3;
pub const EV_CANCEL: u8 = 4;
pub const EV_DONE: u8 = 5;
pub const EV_ERROR: u8 = 6;
pub const EV_NOTE: u8 = 7;

/// The client's turn: `IDLE`, then `STARTING` (the `start` is out, no `listening` yet), `STREAMING` (frames go up),
/// `WAITING` (the end or the server's `endpoint` came; the server works) and back to `IDLE` with the `done`, or with the
/// `note` for a voice note, which frees the device while the answer is followed in the background. One turn at a time: only
/// an idle client takes a press. A cancel or an error ends any turn. A `done` or `error` that carries a note's `shareId`
/// belongs to the note table and never reaches this function.
pub fn step(p: u8, ev: u8) -> u8 //@ret
//@ requires p <= 3, ev <= 7,
//@ ensures
//@     ret <= 3,
//@     (ev == EV_CANCEL || ev == EV_ERROR) ==> ret == IDLE,
//@     ret == STARTING ==> p == STARTING || (p == IDLE && ev == EV_PRESS),
//@     ret == STREAMING ==> p == STREAMING || (p == STARTING && ev == EV_LISTENING),
//@     ret == WAITING ==> p == WAITING || (p == STREAMING && (ev == EV_END || ev == EV_ENDPOINT)),
//@     (p == IDLE && ev != EV_PRESS) ==> ret == IDLE,
//@     (ev == EV_END || ev == EV_ENDPOINT || ev == EV_DONE) ==> ret != STREAMING,
{
    if ev == EV_CANCEL || ev == EV_ERROR {
        return IDLE;
    }
    if p == IDLE {
        if ev == EV_PRESS {
            STARTING
        } else {
            IDLE
        }
    } else if p == STARTING {
        if ev == EV_LISTENING {
            STREAMING
        } else {
            STARTING
        }
    } else if p == STREAMING {
        if ev == EV_END || ev == EV_ENDPOINT {
            WAITING
        } else if ev == EV_DONE {
            IDLE
        } else {
            STREAMING
        }
    } else if ev == EV_DONE || ev == EV_NOTE {
        IDLE
    } else {
        WAITING
    }
}

pub const FRAME_DROP: u8 = 0;
pub const FRAME_HOLD: u8 = 1;
pub const FRAME_SEND: u8 = 2;

/// What happens to a frame the microphone delivers: sent only while streaming, held until the server's `listening` while
/// starting (the first samples are not lost), dropped otherwise.
pub fn frame_action(p: u8) -> u8 //@ret
//@ requires p <= 3,
//@ ensures
//@     ret == FRAME_SEND <==> p == STREAMING,
//@     ret == FRAME_HOLD <==> p == STARTING,
//@     ret == FRAME_DROP <==> (p == IDLE || p == WAITING),
{
    if p == STREAMING {
        FRAME_SEND
    } else if p == STARTING {
        FRAME_HOLD
    } else {
        FRAME_DROP
    }
}

/// How many bytes a hold buffer of `held` bytes may take of a frame of `add` bytes under a cap of `cap`.
pub fn hold_room(held: usize, add: usize, cap: usize) -> usize //@ret
//@ requires held <= cap,
//@ ensures ret <= add, held + ret <= cap, ret < add ==> held + ret == cap,
{
    if add <= cap - held {
        add
    } else {
        cap - held
    }
}

/// The voice notes sent and not yet answered (the server watches at most 4 per connection, and drops the oldest beyond that).
/// Keys are not the share ids themselves: the caller hands out a non-zero number per share id. Non-zero keys come first, oldest
/// first, and no key twice.
//@ pub open spec fn notes_has(n: Notes, k: u32) -> bool { k != 0 && (n.k0 == k || n.k1 == k || n.k2 == k || n.k3 == k) }
//@ pub open spec fn notes_count(n: Notes) -> int { (if n.k0 != 0 { 1int } else { 0int }) + (if n.k1 != 0 { 1int } else { 0int }) + (if n.k2 != 0 { 1int } else { 0int }) + (if n.k3 != 0 { 1int } else { 0int }) }
//@ pub open spec fn notes_ok(n: Notes) -> bool {
//@     (n.k0 == 0 ==> n.k1 == 0) && (n.k1 == 0 ==> n.k2 == 0) && (n.k2 == 0 ==> n.k3 == 0)
//@     && (n.k1 != 0 ==> n.k1 != n.k0) && (n.k2 != 0 ==> n.k2 != n.k0 && n.k2 != n.k1)
//@     && (n.k3 != 0 ==> n.k3 != n.k0 && n.k3 != n.k1 && n.k3 != n.k2)
//@ }
#[derive(Clone, Copy, PartialEq, Eq, Debug)] //@omit
pub struct Notes {
    pub k0: u32,
    pub k1: u32,
    pub k2: u32,
    pub k3: u32,
}

pub const NO_NOTES: Notes = Notes { k0: 0, k1: 0, k2: 0, k3: 0 };

pub fn notes_contains(n: Notes, k: u32) -> bool //@ret
//@ ensures ret == notes_has(n, k),
{
    k != 0 && (n.k0 == k || n.k1 == k || n.k2 == k || n.k3 == k)
}

/// A new note. With four already waiting the oldest is dropped and handed back (the server stopped watching it), else 0.
/// Nothing else is lost, and the new one is in.
pub fn notes_add(n: Notes, k: u32) -> (Notes, u32) //@ret
//@ requires notes_ok(n), k != 0, !notes_has(n, k),
//@ ensures
//@     notes_ok(ret.0),
//@     notes_has(ret.0, k),
//@     notes_count(ret.0) <= 4,
//@     ret.1 == 0 ==> notes_count(ret.0) == notes_count(n) + 1,
//@     ret.1 != 0 ==> notes_has(n, ret.1) && !notes_has(ret.0, ret.1) && notes_count(n) == 4 && notes_count(ret.0) == 4,
//@     forall|j: u32| #![auto] j != ret.1 && notes_has(n, j) ==> notes_has(ret.0, j),
//@     forall|j: u32| #![auto] notes_has(ret.0, j) ==> j == k || notes_has(n, j),
{
    if n.k0 == 0 {
        (Notes { k0: k, k1: 0, k2: 0, k3: 0 }, 0)
    } else if n.k1 == 0 {
        (Notes { k0: n.k0, k1: k, k2: 0, k3: 0 }, 0)
    } else if n.k2 == 0 {
        (Notes { k0: n.k0, k1: n.k1, k2: k, k3: 0 }, 0)
    } else if n.k3 == 0 {
        (Notes { k0: n.k0, k1: n.k1, k2: n.k2, k3: k }, 0)
    } else {
        (Notes { k0: n.k1, k1: n.k2, k2: n.k3, k3: k }, n.k0)
    }
}

/// The note's `done` or `error` came: it goes. The others stay, in order.
pub fn notes_remove(n: Notes, k: u32) -> Notes //@ret
//@ requires notes_ok(n),
//@ ensures
//@     notes_ok(ret),
//@     !notes_has(ret, k),
//@     notes_count(ret) == notes_count(n) - (if notes_has(n, k) { 1int } else { 0int }),
//@     forall|j: u32| #![auto] j != k && notes_has(n, j) ==> notes_has(ret, j),
//@     forall|j: u32| #![auto] notes_has(ret, j) ==> notes_has(n, j),
{
    if k == 0 {
        n
    } else if n.k0 == k {
        Notes { k0: n.k1, k1: n.k2, k2: n.k3, k3: 0 }
    } else if n.k1 == k {
        Notes { k0: n.k0, k1: n.k2, k2: n.k3, k3: 0 }
    } else if n.k2 == k {
        Notes { k0: n.k0, k1: n.k1, k2: n.k3, k3: 0 }
    } else if n.k3 == k {
        Notes { k0: n.k0, k1: n.k1, k2: n.k2, k3: 0 }
    } else {
        n
    }
}
