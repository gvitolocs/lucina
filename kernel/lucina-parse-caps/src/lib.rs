// SPDX-License-Identifier: MIT OR Apache-2.0

//! The guards every Juno port of a Craft app puts in front of an upstream file parser (`artcraft/`,
//! `docs/research/juno-artcraft.md`): a byte budget, a read that never holds more than its cap, a zip entry check
//! (declared size, compression ratio, archive total), a count of items, the size of a picture or grid, an offset and
//! length inside a buffer, and a nesting level for recursive descent. They decide from numbers only, before the upstream
//! parser allocates or recurses, so a hostile file is refused for what it declares and again for what it really holds.
//!
//! Every function is pure, total and `no_std`, with no dependencies, and computes nothing that can overflow for any
//! input. Verus proves this very file (`verification/juno-parse-caps/rules.rs`, D-011): what each function promises is in
//! its `ensures`, stated over mathematical integers; the models there prove that a budget never hands out more than it
//! started with, that a capped read never holds more than its cap plus one byte, that an archive walked through
//! [`zip_entry`] and [`count_next`] never declares more than its total or holds more than its entry count, and that a
//! recursion through [`enter`] never goes deeper than its limit. `verification/juno-parse-caps/mutants.sh` plants a bug
//! in each and Verus must refuse every one.
#![no_std]
#![allow(unexpected_cfgs)]

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

/// Takes `n` from a budget with `left` remaining: the new remainder, or `None` (refuse) when `n` is more than is left.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        match r {
            Some(l) => n <= left && l == left - n,
            None => n > left,
        },
))]
pub fn take(left: u64, n: u64) -> Option<u64> {
    if n <= left { Some(left - n) } else { None }
}

/// `a × b` when it is at most `cap`, else `None`. Never overflows: the product is only computed once it is known to fit.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        match r {
            Some(p) => p as int == a as int * b as int && p <= cap,
            None => a as int * b as int > cap as int,
        },
))]
pub fn product(a: u64, b: u64, cap: u64) -> Option<u64> {
    if a == 0 || b == 0 {
        return Some(0);
    }
    if b > cap / a {
        #[cfg(verus_keep_ghost)]
        proof! {
            assert(a as int * b as int > cap as int) by (nonlinear_arith)
                requires a > 0, b > cap / a;
        }
        return None;
    }
    #[cfg(verus_keep_ghost)]
    proof! {
        assert(a as int * b as int <= cap as int) by (nonlinear_arith)
            requires a > 0, b <= cap / a;
    }
    Some(a * b)
}

/// The bytes of a `w × h` picture (or grid) of `per` bytes each, when that is at most `cap`, else `None`.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        match r {
            Some(p) => p as int == w as int * h as int * per as int && p <= cap,
            None => w as int * h as int * per as int > cap as int,
        },
))]
pub fn area(w: u64, h: u64, per: u64, cap: u64) -> Option<u64> {
    if per == 0 {
        #[cfg(verus_keep_ghost)]
        proof! {
            assert(w as int * h as int * per as int == 0) by (nonlinear_arith) requires per == 0;
        }
        return Some(0);
    }
    match product(w, h, cap) {
        Some(wh) => product(wh, per, cap),
        None => {
            #[cfg(verus_keep_ghost)]
            proof! {
                assert(w as int * h as int * per as int >= w as int * h as int) by (nonlinear_arith)
                    requires per >= 1, w >= 0, h >= 0;
            }
            None
        }
    }
}

/// The end of `len` bytes from `off` when they lie inside a buffer of `total` bytes, else `None`. Never overflows.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        match r {
            Some(end) => end as int == off as int + len as int && end <= total,
            None => off as int + len as int > total as int,
        },
))]
pub fn range(off: u64, len: u64, total: u64) -> Option<u64> {
    if off <= total && len <= total - off { Some(off + len) } else { None }
}

/// One more item when `count` items are already there and at most `cap` are allowed: the new count, or `None` (refuse).
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        match r {
            Some(c) => count < cap && c == count + 1,
            None => count >= cap,
        },
))]
pub fn count_next(count: u64, cap: u64) -> Option<u64> {
    if count < cap { Some(count + 1) } else { None }
}

/// One level deeper in a recursive descent at `depth`, with at most `max` levels: the new depth, or `None` (refuse).
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        match r {
            Some(d) => depth < max && d == depth + 1,
            None => depth >= max,
        },
))]
pub fn enter(depth: u32, max: u32) -> Option<u32> {
    if depth < max { Some(depth + 1) } else { None }
}

/// A zip (or other archive) entry that declares `declared` bytes unpacked from `compressed` packed bytes: accepted when it
/// is at most `max_entry`, at most `max_ratio` times its packed size, and at most the archive budget `total_left`; the new
/// archive budget, or `None` (refuse).
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        match r {
            Some(l) => declared <= max_entry
                && declared as int <= compressed as int * max_ratio as int
                && declared <= total_left
                && l == total_left - declared,
            None => declared > max_entry
                || declared as int > compressed as int * max_ratio as int
                || declared > total_left,
        },
))]
pub fn zip_entry(declared: u64, compressed: u64, max_entry: u64, max_ratio: u64, total_left: u64) -> Option<u64> {
    if declared > max_entry {
        return None;
    }
    // compressed × max_ratio, or u64::MAX when it does not fit (then it is above any declared size).
    let allowed = match product(compressed, max_ratio, u64::MAX) {
        Some(p) => p,
        None => u64::MAX,
    };
    if declared > allowed {
        return None;
    }
    take(total_left, declared)
}

/// More unpacked output: `produced` bytes so far, `chunk` more. The new total while it stays within what the entry
/// `declared`, or `None` (refuse: the entry holds more than it said, a lying header).
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        match r {
            Some(p) => p as int == produced as int + chunk as int && p <= declared,
            None => produced as int + chunk as int > declared as int,
        },
))]
pub fn inflate_more(produced: u64, chunk: u64, declared: u64) -> Option<u64> {
    range(produced, chunk, declared)
}

/// How many bytes a capped read may ask for next, having `have` bytes, wanting `want`, with a cap of `cap` bytes: at
/// most enough to reach `cap + 1`, so a source longer than the cap is seen as such (one byte over) without holding more.
/// `cap` must leave room for that one byte.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires
        cap < u64::MAX,
        have <= cap + 1,
    ensures
        r <= want,
        have + r <= cap + 1,
        r == want || have + r == cap + 1,
))]
pub fn read_next(have: u64, want: u64, cap: u64) -> u64 {
    let room = cap + 1 - have;
    if want < room { want } else { room }
}

/// Whether a capped read that ended with `total` bytes must be refused: more than `cap`.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == (total > cap),
))]
pub fn over(total: u64, cap: u64) -> bool {
    total > cap
}

#[cfg(test)]
mod tests;
