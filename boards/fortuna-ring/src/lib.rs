//! A single-producer single-consumer ring between the DSi's two CPUs: the microphone's samples, written by the
//! ARM7's MIC FIFO IRQ and read by the ARM9's voice-note recorder. `no_std`, no allocation: the buffer and the
//! [`Shared`] counters live in main RAM the caller sets aside.
//!
//! The counter arithmetic is in [`rules`], checked by Verus in the shipped file. The order between the CPUs is
//! checked by TLC in `fortuna/formal/FortunaMicRing.tla`, and this code keeps to it:
//!
//! **CROSS-CPU RULES**
//! 1. The producer writes the samples' slots first and publishes the write counter after them.
//! 2. It never writes a slot the consumer has not taken: what does not fit is dropped and counted.
//! 3. The consumer reads the write counter, invalidates the data cache over the slots it is about to read, reads
//!    them, then publishes the read counter.
//! 4. The counters are read and written whole (32-bit loads and stores, `Ordering::Relaxed`), each by one side only.
//!    Order comes from program order plus `compiler_fence`: the DSi's ARM946E-S and ARM7TDMI are in-order and drain
//!    their write buffers in order, and the cache maintenance below moves data between them. No `Acquire`/`Release`
//!    atomics: on ARMv5 LLVM lowers them to `__sync_*` libcalls, and a compare-and-swap made "atomic" by masking the
//!    ARM9's IRQs is not atomic against the ARM7 (a CAS(wr, 0, 0) that writes 0 back can undo the ARM7's publish).
//!    This makes `Ring` a ring for the DSi's CPUs, not a general one for weakly ordered CPUs. On the ARM9 the [`Shared`] block is
//!    alone in its cache line (`align(32)`); the consumer invalidates that line before reading `wr` and cleans it
//!    after writing `rd` ([`CacheOps`]). The ARM7 has no cache.
//!
//! A full ring drops new samples rather than overwriting old ones: a voice note then has a gap (counted in
//! [`Shared::dropped`]), never a stale or torn sample.

#![no_std]

#[cfg(test)]
extern crate std;

// The verified file keeps the `%` form Verus checks (u64::is_multiple_of has no Verus spec).
#[allow(clippy::manual_is_multiple_of)]
pub mod rules;

use core::sync::atomic::{compiler_fence, AtomicU32, Ordering};

/// How a side reads the other side's counter and publishes its own. On the DSi (`target_vendor = "nintendo"`):
/// plain 32-bit loads and stores, ordered by program order and `compiler_fence` (CROSS-CPU RULES, 4). Elsewhere (the
/// host's threaded test): `Acquire`/`Release`, so the test is well defined under Rust's memory model.
#[cfg(target_vendor = "nintendo")]
const ACQUIRE: Ordering = Ordering::Relaxed;
#[cfg(target_vendor = "nintendo")]
const RELEASE: Ordering = Ordering::Relaxed;
#[cfg(not(target_vendor = "nintendo"))]
const ACQUIRE: Ordering = Ordering::Acquire;
#[cfg(not(target_vendor = "nintendo"))]
const RELEASE: Ordering = Ordering::Release;

/// The counters both CPUs see. Put it in main RAM, alone in its cache line.
#[repr(C, align(32))]
pub struct Shared {
    /// Samples handed over (written by the producer only).
    pub wr: AtomicU32,
    /// Samples taken (written by the consumer only).
    pub rd: AtomicU32,
    /// Samples that did not fit (written by the producer only).
    pub dropped: AtomicU32,
}

impl Shared {
    pub const fn new() -> Self {
        Shared { wr: AtomicU32::new(0), rd: AtomicU32::new(0), dropped: AtomicU32::new(0) }
    }
}

impl Default for Shared {
    fn default() -> Self {
        Self::new()
    }
}

/// What a CPU with a data cache must do around shared memory. The ARM7 and the host need nothing ([`NoCache`]);
/// the ARM9 passes libnds' `DC_InvalidateRange` and `DC_FlushRange`.
pub trait CacheOps {
    /// Drop cached copies of `len` bytes from `ptr`, so the next read comes from RAM.
    fn invalidate(&self, ptr: *const u8, len: usize);
    /// Write cached changes of `len` bytes from `ptr` back to RAM.
    fn clean(&self, ptr: *const u8, len: usize);
}

/// No data cache (the ARM7, the host).
pub struct NoCache;

impl CacheOps for NoCache {
    fn invalidate(&self, _ptr: *const u8, _len: usize) {}
    fn clean(&self, _ptr: *const u8, _len: usize) {}
}

/// One side's view of a ring: the counters, the slots, the capacity.
pub struct Ring<'a, T: Copy> {
    shared: &'a Shared,
    data: *mut T,
    cap: u32,
}

// The ring is shared between exactly one producer and one consumer by design (CROSS-CPU RULES).
unsafe impl<T: Copy + Send> Send for Ring<'_, T> {}
unsafe impl<T: Copy + Send> Sync for Ring<'_, T> {}

impl<'a, T: Copy> Ring<'a, T> {
    /// A view of `cap` slots at `data`.
    ///
    /// # Safety
    /// `data` points to `cap` slots of `T` that stay valid while the ring is used, and only one producer and one
    /// consumer use the ring at a time. `cap` is a power of two up to `rules::MAX_CAP` (checked).
    pub unsafe fn from_raw(shared: &'a Shared, data: *mut T, cap: u32) -> Self {
        assert!(rules::cap_ok(cap), "the ring size must be a power of two up to rules::MAX_CAP");
        Ring { shared, data, cap }
    }

    /// A view of a slice (the host, tests, and a ring in static memory).
    pub fn new(shared: &'a Shared, data: &'a mut [T]) -> Self {
        assert!(data.len() <= rules::MAX_CAP as usize && rules::cap_ok(data.len() as u32), "the ring size must be a power of two up to rules::MAX_CAP");
        Ring { shared, data: data.as_mut_ptr(), cap: data.len() as u32 }
    }

    pub fn capacity(&self) -> u32 {
        self.cap
    }

    /// Samples waiting (either side; a snapshot).
    pub fn len(&self) -> u32 {
        rules::used(self.shared.wr.load(ACQUIRE), self.shared.rd.load(ACQUIRE))
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Samples dropped because the ring was full.
    pub fn dropped(&self) -> u32 {
        self.shared.dropped.load(Ordering::Relaxed)
    }

    /// The producer: hands over as many of `src` as fit (rule 1 and 2); returns how many. The rest is counted as
    /// dropped.
    pub fn push_slice(&self, src: &[T]) -> usize {
        let wr = self.shared.wr.load(Ordering::Relaxed);
        let rd = self.shared.rd.load(ACQUIRE);
        compiler_fence(Ordering::Acquire);
        let n = src.len().min(u32::MAX as usize) as u32;
        let k = rules::room(wr, rd, self.cap, n);
        let (slot, first) = rules::runs(wr, self.cap, k);
        // SAFETY: `runs` keeps both runs inside the `cap` slots and apart; `room` keeps them clear of unread slots.
        unsafe {
            core::ptr::copy_nonoverlapping(src.as_ptr(), self.data.add(slot as usize), first as usize);
            core::ptr::copy_nonoverlapping(src.as_ptr().add(first as usize), self.data, (k - first) as usize);
        }
        compiler_fence(Ordering::Release);
        self.shared.wr.store(rules::after_push(wr, rd, k), RELEASE);
        if k < n {
            let d = self.shared.dropped.load(Ordering::Relaxed);
            self.shared.dropped.store(d.wrapping_add(n - k), Ordering::Relaxed);
        }
        k as usize
    }

    /// The consumer: takes up to `dst.len()` samples, oldest first (rule 3); returns how many.
    pub fn pop_into(&self, dst: &mut [T], cache: &impl CacheOps) -> usize {
        let hdr = self.shared as *const Shared as *const u8;
        cache.invalidate(hdr, core::mem::size_of::<Shared>());
        let wr = self.shared.wr.load(ACQUIRE);
        let rd = self.shared.rd.load(Ordering::Relaxed);
        compiler_fence(Ordering::Acquire);
        let n = dst.len().min(u32::MAX as usize) as u32;
        let k = rules::take(wr, rd, n);
        let (slot, first) = rules::runs(rd, self.cap, k);
        let size = core::mem::size_of::<T>();
        // SAFETY: as in push_slice; `take` keeps the runs within what the producer handed over.
        unsafe {
            cache.invalidate(self.data.add(slot as usize) as *const u8, first as usize * size);
            cache.invalidate(self.data as *const u8, (k - first) as usize * size);
            core::ptr::copy_nonoverlapping(self.data.add(slot as usize), dst.as_mut_ptr(), first as usize);
            core::ptr::copy_nonoverlapping(self.data, dst.as_mut_ptr().add(first as usize), (k - first) as usize);
        }
        compiler_fence(Ordering::Release);
        self.shared.rd.store(rules::after_pop(wr, rd, k), RELEASE);
        cache.clean(hdr, core::mem::size_of::<Shared>());
        k as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::vec;
    use std::vec::Vec;

    #[test]
    fn fills_drops_and_wraps() {
        let sh = Shared::new();
        let mut buf = [0i16; 4];
        let r = Ring::new(&sh, &mut buf);
        assert_eq!(r.push_slice(&[1, 2, 3]), 3);
        assert_eq!(r.push_slice(&[4, 5, 6]), 1, "only one slot is free");
        assert_eq!(r.dropped(), 2);
        let mut out = [0i16; 3];
        assert_eq!(r.pop_into(&mut out, &NoCache), 3);
        assert_eq!(out, [1, 2, 3]);
        assert_eq!(r.push_slice(&[7, 8, 9]), 3, "wraps around the end");
        let mut out = [0i16; 8];
        assert_eq!(r.pop_into(&mut out, &NoCache), 4);
        assert_eq!(&out[..4], &[4, 7, 8, 9]);
        assert!(r.is_empty());
    }

    #[test]
    #[should_panic(expected = "power of two")]
    fn refuses_a_size_that_does_not_divide_2_pow_32() {
        let sh = Shared::new();
        let mut buf = [0u8; 3];
        let _ = Ring::new(&sh, &mut buf);
    }

    #[test]
    fn counters_wrap_at_2_pow_32() {
        let sh = Shared::new();
        sh.wr.store(u32::MAX - 1, Ordering::Relaxed);
        sh.rd.store(u32::MAX - 1, Ordering::Relaxed);
        let mut buf = [0u8; 4];
        let r = Ring::new(&sh, &mut buf);
        for round in 0..10u8 {
            assert_eq!(r.push_slice(&[round, round + 1]), 2);
            let mut out = [0u8; 2];
            assert_eq!(r.pop_into(&mut out, &NoCache), 2);
            assert_eq!(out, [round, round + 1]);
        }
    }

    /// Random pushes and pops against a VecDeque that never drops more than the ring must.
    #[test]
    fn matches_a_model() {
        let mut x = 0x9e37_79b9u32;
        let mut rnd = |m: u32| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x % m
        };
        for cap in [1usize, 2, 4, 8, 16] {
            let sh = Shared::new();
            sh.wr.store(u32::MAX - 20, Ordering::Relaxed);
            sh.rd.store(u32::MAX - 20, Ordering::Relaxed);
            let mut buf = vec![0u32; cap];
            let r = Ring::new(&sh, &mut buf);
            let mut model: VecDeque<u32> = VecDeque::new();
            let mut next = 0u32;
            let mut dropped = 0u32;
            for _ in 0..5000 {
                if rnd(2) == 0 {
                    let n = rnd(cap as u32 + 3) as usize;
                    let src: Vec<u32> = (0..n as u32).map(|i| next + i).collect();
                    next += n as u32;
                    let k = r.push_slice(&src);
                    assert_eq!(k, n.min(cap - model.len()));
                    model.extend(&src[..k]);
                    dropped += (n - k) as u32;
                } else {
                    let mut out = vec![0u32; rnd(cap as u32 + 3) as usize];
                    let k = r.pop_into(&mut out, &NoCache);
                    let want: Vec<u32> = model.drain(..k.min(model.len())).collect();
                    assert_eq!(&out[..k], &want[..]);
                }
                assert_eq!(r.len() as usize, model.len());
                assert_eq!(r.dropped(), dropped);
            }
        }
    }

    /// Two real threads, one producer and one consumer: every sample arrives once, in order, or is counted as
    /// dropped.
    #[test]
    fn two_threads() {
        static SH: Shared = Shared::new();
        let buf: &'static mut [u32] = std::boxed::Box::leak(vec![0u32; 64].into_boxed_slice());
        let ptr = buf.as_mut_ptr() as usize;
        const TOTAL: u32 = 200_000;
        let prod = std::thread::spawn(move || {
            let r = unsafe { Ring::from_raw(&SH, ptr as *mut u32, 64) };
            let mut i = 0u32;
            while i < TOTAL {
                let chunk: Vec<u32> = (i..(i + 7).min(TOTAL)).collect();
                r.push_slice(&chunk);
                i += chunk.len() as u32;
            }
        });
        let r = unsafe { Ring::from_raw(&SH, ptr as *mut u32, 64) };
        let mut last: Option<u32> = None;
        let mut got = 0u32;
        let mut out = [0u32; 13];
        loop {
            let k = r.pop_into(&mut out, &NoCache);
            for &v in &out[..k] {
                assert!(last.is_none_or(|l| v > l), "{v} after {last:?}");
                last = Some(v);
                got += 1;
            }
            if k == 0 && prod.is_finished() && r.is_empty() {
                break;
            }
        }
        prod.join().unwrap();
        assert_eq!(got + r.dropped(), TOTAL);
    }
}
