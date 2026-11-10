// The counter rules of Fortuna's single-producer single-consumer ring (the microphone's samples, ARM7 IRQ to
// ARM9): how many samples the ring holds, how many more fit, how the 32-bit counters move, and where a run of
// slots lies in the buffer. Verus checks this very file: `//@` lines hold the proof, see scripts/verify-shipped.sh
// (D-011). lib.rs calls these functions for every push and pop and keeps no copy of the rules.
//
// The counters count samples since the start and wrap at 2^32; the slot of counter c is c % cap. The ring holds
// `used(wr, rd)` samples, never more than `cap`: the producer only adds what `room` allows and the consumer only
// takes what `used` reports. The cross-CPU order (slot before counter, invalidate before read) is checked by TLC in
// fortuna/formal/FortunaMicRing.tla.

/// The largest ring: far beyond the microphone's needs (a second of 10.9 kHz samples is 10 913).
pub const MAX_CAP: u32 = 0x1_0000;

//@ pub open spec fn spec_cap_ok(cap: u32) -> bool {
//@     0 < cap && cap <= MAX_CAP && 0x1_0000_0000int % (cap as int) == 0
//@ }

/// Whether `cap` works as a ring size: 1 ..= MAX_CAP and a divisor of 2^32 (a power of two). Only then does the
/// slot `c % cap` move on by one when the counter wraps from 2^32 - 1 to 0 ([`lemma_slot_continuous`]); with 3
/// slots, counter 2^32 - 1 and counter 0 would both be slot 0 (the host test caught exactly that).
pub fn cap_ok(cap: u32) -> bool //@ret
//@ ensures ret == spec_cap_ok(cap),
{
    cap > 0 && cap <= MAX_CAP && 0x1_0000_0000u64 % (cap as u64) == 0
}

// The slot after k more samples is k slots on, also across the counter wrap.
//@ pub proof fn lemma_slot_continuous(c: u32, k: u32, cap: u32)
//@     requires spec_cap_ok(cap),
//@     ensures ((c as int + k as int) % 0x1_0000_0000int) % (cap as int) == (c as int % (cap as int) + k as int) % (cap as int),
//@ {
//@     let m = cap as int;
//@     let q = 0x1_0000_0000int / m;
//@     vstd::arithmetic::div_mod::lemma_fundamental_div_mod(0x1_0000_0000int, m);
//@     assert(m * q == 0x1_0000_0000int);
//@     vstd::arithmetic::div_mod::lemma_mod_mod(c as int + k as int, m, q);
//@     vstd::arithmetic::div_mod::lemma_add_mod_noop(c as int, k as int, m);
//@     vstd::arithmetic::div_mod::lemma_add_mod_noop(c as int % m, k as int, m);
//@     vstd::arithmetic::div_mod::lemma_mod_twice(c as int, m);
//@ }

//@ pub open spec fn spec_used(wr: u32, rd: u32) -> int {
//@     if wr >= rd { (wr - rd) as int } else { wr as int + 0x1_0000_0000 - rd as int }
//@ }

/// Samples handed over and not yet taken: `wr - rd` on the wrapping counters.
pub fn used(wr: u32, rd: u32) -> u32 //@ret
//@ ensures ret as int == spec_used(wr, rd),
{
    if wr >= rd {
        wr - rd
    } else {
        (u32::MAX - rd) + wr + 1
    }
}

/// How many of `n` new samples fit: all of them when there is room, else the free slots (the producer drops the
/// rest and counts them).
pub fn room(wr: u32, rd: u32, cap: u32, n: u32) -> u32 //@ret
//@ requires 0 < cap <= MAX_CAP, spec_used(wr, rd) <= cap as int,
//@ ensures ret <= n, ret as int + spec_used(wr, rd) <= cap as int,
//@     n as int + spec_used(wr, rd) <= cap as int ==> ret == n,
{
    let free = cap - used(wr, rd);
    if n < free {
        n
    } else {
        free
    }
}

/// How many samples the consumer takes when it has space for `n`.
pub fn take(wr: u32, rd: u32, n: u32) -> u32 //@ret
//@ ensures ret <= n, ret as int <= spec_used(wr, rd),
//@     (n as int <= spec_used(wr, rd) ==> ret == n), (spec_used(wr, rd) <= n as int ==> ret as int == spec_used(wr, rd)),
{
    let u = used(wr, rd);
    if n < u {
        n
    } else {
        u
    }
}

/// The write counter after `k` more samples: the ring then holds `k` more.
pub fn after_push(wr: u32, _rd: u32, k: u32) -> u32 //@ret
//@ requires spec_used(wr, _rd) + k as int <= MAX_CAP as int,
//@ ensures spec_used(ret, _rd) == spec_used(wr, _rd) + k as int, ret as int == (wr as int + k as int) % 0x1_0000_0000int,
{
    if k <= u32::MAX - wr {
        wr + k
    } else {
        k - (u32::MAX - wr) - 1
    }
}

/// The read counter after taking `k` samples: the ring then holds `k` fewer.
pub fn after_pop(_wr: u32, rd: u32, k: u32) -> u32 //@ret
//@ requires k as int <= spec_used(_wr, rd),
//@ ensures spec_used(_wr, ret) == spec_used(_wr, rd) - k as int, ret as int == (rd as int + k as int) % 0x1_0000_0000int,
{
    if k <= u32::MAX - rd {
        rd + k
    } else {
        k - (u32::MAX - rd) - 1
    }
}

/// Where `k` slots from counter `c` lie in the buffer: `(slot, first)`, the first run `slot .. slot + first`, and the
/// rest (`k - first` slots) from slot 0. Both runs are inside the buffer and do not overlap.
pub fn runs(c: u32, cap: u32, k: u32) -> (u32, u32) //@ret
//@ requires 0 < cap <= MAX_CAP, k <= cap,
//@ ensures ret.0 < cap, ret.1 <= k, ret.0 + ret.1 <= cap,
//@     ret.0 == c % cap, (ret.1 < k ==> ret.0 + ret.1 == cap), k - ret.1 <= ret.0 || ret.1 == k,
{
    let s = c % cap;
    let left = cap - s;
    if k < left {
        (s, k)
    } else {
        (s, left)
    }
}
