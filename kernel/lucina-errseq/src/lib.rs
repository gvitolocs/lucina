// SPDX-License-Identifier: GPL-2.0-only
//! Linux's `errseq_t` (`lib/errseq.c`, `include/linux/errseq.h`, tag v6.12), written again for its behaviour.
//!
//! An [`ErrSeq`] records errors in one place. Any number of holders each keep a *sample* (a plain `u32`) and ask later
//! whether an error has been recorded since. The 32-bit value is laid out as:
//!
//! * bits 0..=11: the errno, as a positive number (`-err`); [`MAX_ERRNO`] = 4095 is the mask;
//! * bit 12: [`SEEN`], set once somebody has looked at the value since it was last recorded;
//! * bits 13..=31: a counter, bumped by [`CTR_INC`].
//!
//! The counter only moves when an error is recorded over a value that was already seen, so errors recorded
//! back to back with nobody looking collapse into one. The counter wraps around at 2^19 steps like Linux's
//! unsigned arithmetic; after a full wrap a stale sample can compare equal to the current value and hide an
//! error (the collision risk Linux documents). All updates are lock-free compare-exchange loops.
//!
//! Licence: this crate re-implements the behaviour of Linux's `lib/errseq.c` (v6.12, commit
//! `adc218676eef25575469234709c2d87185ca223a`), whose licence is GPL-2.0 (its SPDX line), so this crate is under
//! GPL-2.0-only; see the README.
//!
//! Errors are negative errnos in `i32`, as in Linux: `set(-5)` records errno 5, and `check*` return `-5` or `0`.
#![no_std]
#![forbid(unsafe_code)]

use core::sync::atomic::{AtomicU32, Ordering};

/// Largest errno that can be stored (Linux `MAX_ERRNO`); also the mask of the errno bits.
pub const MAX_ERRNO: u32 = 4095;
/// Bit position of the SEEN flag: `ilog2(MAX_ERRNO + 1)`.
pub const SHIFT: u32 = 12;
/// The "has been seen" flag.
pub const SEEN: u32 = 1 << SHIFT;
/// The lowest bit of the counter.
pub const CTR_INC: u32 = 1 << (SHIFT + 1);

const _: () = assert!(MAX_ERRNO + 1 == 1 << SHIFT);

/// An `errseq_t`. A new one is zero: "never had an error".
#[derive(Debug, Default)]
pub struct ErrSeq(AtomicU32);

impl ErrSeq {
    /// A fresh value, all zero.
    pub const fn new() -> Self {
        Self(AtomicU32::new(0))
    }

    /// The raw 32-bit value (for tests and logs; not a sample, see [`sample`](Self::sample)).
    pub fn load(&self) -> u32 {
        self.0.load(Ordering::Relaxed)
    }

    /// Record `err` (between -1 and -4095), overwriting any earlier error. The counter is bumped only if the
    /// old value had been seen; the new value is stored with SEEN clear. `0` and anything outside the range
    /// is refused and the current value returned unchanged, as Linux does (it also warns). Returns the previous
    /// value (the new one if another writer got there with the same result first); not to be used as a sample.
    pub fn set(&self, err: i32) -> u32 {
        let mut old = self.0.load(Ordering::Relaxed);
        if err == 0 || (err as u32).wrapping_neg() > MAX_ERRNO {
            return old;
        }
        let errno = (err as u32).wrapping_neg();
        loop {
            let mut new = (old & !(MAX_ERRNO | SEEN)) | errno;
            if old & SEEN != 0 {
                new = new.wrapping_add(CTR_INC);
            }
            if new == old {
                return new;
            }
            match self
                .0
                .compare_exchange(old, new, Ordering::SeqCst, Ordering::Relaxed)
            {
                Ok(cur) => return cur,
                Err(cur) if cur == new => return cur,
                Err(cur) => old = cur,
            }
        }
    }

    /// The value a new holder records. If the current value has not been seen, 0 is returned so the holder
    /// still sees that error the next time it checks; if it has been seen, the holder starts clean after it.
    pub fn sample(&self) -> u32 {
        let old = self.0.load(Ordering::Relaxed);
        if old & SEEN == 0 { 0 } else { old }
    }

    /// Has an error been recorded since `since`? Returns the latest error (negative errno) or 0. Does not
    /// advance anything and does not mark the value seen.
    pub fn check(&self, since: u32) -> i32 {
        let cur = self.0.load(Ordering::Relaxed);
        if cur == since {
            return 0;
        }
        -((cur & MAX_ERRNO) as i32)
    }

    /// Like [`check`](Self::check), but on a change sets SEEN on the shared value (one attempt; losing the
    /// race is fine, as in Linux) and advances `*since` to the value with SEEN set. Callers serialise their own
    /// use of `since`.
    pub fn check_and_advance(&self, since: &mut u32) -> i32 {
        let old = self.0.load(Ordering::Relaxed);
        if old == *since {
            return 0;
        }
        let new = old | SEEN;
        if new != old {
            let _ = self
                .0
                .compare_exchange(old, new, Ordering::SeqCst, Ordering::Relaxed);
        }
        *since = new;
        -((new & MAX_ERRNO) as i32)
    }
}

#[cfg(test)]
mod tests;
