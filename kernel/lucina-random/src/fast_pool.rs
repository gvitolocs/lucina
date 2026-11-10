//! The timing of interrupts on its way to the pool: `fast_mix` and `add_interrupt_randomness` of Linux's `random.c`. One of these
//! per CPU, cheap enough for every interrupt.
//!
//! This is SipHash-1-x from an empty key, with no security of its own: its inputs are not chosen by an attacker to matter, and
//! the pool's BLAKE2s is what protects what comes out.

/// `s` is the four words of SipHash's state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FastPool {
    s: [u64; 4],
    count: u32,
    last: u64,
}

/// What goes into the pool, and how many bits it may count for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Flush {
    /// Half of the state: the other half stays, as the key of what comes next.
    pub pool: [u64; 2],
    pub credit: u32,
}

/// `SIPHASH_PERMUTATION`.
fn sip_round(s: &mut [u64; 4]) {
    s[0] = s[0].wrapping_add(s[1]);
    s[1] = s[1].rotate_left(13);
    s[1] ^= s[0];
    s[0] = s[0].rotate_left(32);
    s[2] = s[2].wrapping_add(s[3]);
    s[3] = s[3].rotate_left(16);
    s[3] ^= s[2];
    s[0] = s[0].wrapping_add(s[3]);
    s[3] = s[3].rotate_left(21);
    s[3] ^= s[0];
    s[2] = s[2].wrapping_add(s[1]);
    s[1] = s[1].rotate_left(17);
    s[1] ^= s[2];
    s[2] = s[2].rotate_left(32);
}

impl FastPool {
    pub const fn new() -> Self {
        Self {
            s: [0; 4],
            count: 0,
            last: 0,
        }
    }

    /// One interrupt: `counter` is the time, `other` whatever else tells it apart. A [`Flush`] every 1024 of them or every second,
    /// whichever comes first. `now` and `ticks_per_s` are on the same clock as `counter`.
    pub fn add(&mut self, counter: u64, other: u64, now: u64, ticks_per_s: u64) -> Option<Flush> {
        self.s[3] ^= counter;
        sip_round(&mut self.s);
        self.s[0] ^= counter;
        self.s[3] ^= other;
        sip_round(&mut self.s);
        self.s[0] ^= other;
        self.count = self.count.saturating_add(1);
        if self.count < 1024 && now.wrapping_sub(self.last) < ticks_per_s {
            return None;
        }
        let flush = Flush {
            pool: [self.s[0], self.s[1]],
            credit: ((self.count & 0xffff) / 64).clamp(1, 128),
        };
        self.count = 0;
        self.last = now;
        Some(flush)
    }
}

impl Default for FastPool {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TICKS: u64 = 19_200_000;

    #[test]
    fn a_flush_every_1024_interrupts() {
        let mut p = FastPool::new();
        let mut last = None;
        for i in 0..1024u64 {
            let flush = p.add(i, 7, 10, TICKS);
            if i < 1023 {
                // `now` stays within the second of `last`: only the count can flush.
                assert_eq!(flush, None);
            }
            last = flush;
        }
        assert_eq!(last.unwrap().credit, 16);
    }

    #[test]
    fn a_flush_every_second_counts_for_one_bit() {
        let mut p = FastPool::new();
        assert_eq!(p.add(1, 7, 1, TICKS), None);
        let flush = p.add(2, 7, TICKS + 1, TICKS).unwrap();
        assert_eq!(flush.credit, 1);
        assert_eq!(p.add(3, 7, TICKS + 2, TICKS), None);
    }

    #[test]
    fn the_state_depends_on_every_input_and_keeps_half_back() {
        let run = |values: &[u64]| {
            let mut p = FastPool::new();
            for v in values {
                p.add(*v, 7, 0, TICKS);
            }
            p.s
        };
        assert_ne!(run(&[1, 2, 3]), run(&[1, 2, 4]));
        assert_ne!(run(&[1, 2, 3]), run(&[2, 1, 3]));
        let mut p = FastPool::new();
        p.add(1, 7, 0, TICKS);
        let flush = p.add(2, 7, TICKS, TICKS).unwrap();
        assert_eq!(flush.pool, [p.s[0], p.s[1]]);
        assert_ne!(p.s[2], 0);
    }
}
