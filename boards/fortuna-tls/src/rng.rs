//! Randomness on a console that has no generator: everything unpredictable the DSi offers goes through SHA-256 into
//! a pool, and the pool seeds ChaCha20 (RFC 8439's cipher as a generator, `rand_chacha`).
//!
//! What goes in (the kernel decides when): the seed file on the SD card, written back from the generator after
//! every seeding so that each boot starts from all the boots before it; the DSi's clock and MAC address; the
//! low bits of the 33 MHz timer at moments that depend on Wi-Fi, the touch screen and the scheduler; microphone
//! samples while a voice note records. A fresh pool is mixed into the generator before every TLS handshake.

use rand_chacha::ChaCha20Rng;
use rand_core::{CryptoRng, RngCore, SeedableRng};
use sha2::{Digest, Sha256};

pub struct Pool {
    hash: Sha256,
    /// Bytes mixed in since the last seeding, for the log ("entropy pool" counts inputs, it cannot measure them).
    pub inputs: u32,
}

impl Default for Pool {
    fn default() -> Self {
        Pool::new()
    }
}

impl Pool {
    pub fn new() -> Pool {
        Pool { hash: Sha256::new(), inputs: 0 }
    }

    pub fn mix(&mut self, bytes: &[u8]) {
        self.hash.update((bytes.len() as u32).to_le_bytes());
        self.hash.update(bytes);
        self.inputs = self.inputs.saturating_add(1);
    }

    pub fn mix_u32(&mut self, v: u32) {
        self.mix(&v.to_le_bytes());
    }
}

pub struct Rng {
    inner: ChaCha20Rng,
}

impl Rng {
    /// A generator from a pool.
    pub fn from_pool(pool: Pool) -> Rng {
        let seed: [u8; 32] = pool.hash.finalize().into();
        Rng { inner: ChaCha20Rng::from_seed(seed) }
    }

    /// Mixes a new pool in: the next state depends on the old state and everything in the pool.
    pub fn reseed(&mut self, mut pool: Pool) {
        let mut old = [0u8; 32];
        self.inner.fill_bytes(&mut old);
        pool.mix(&old);
        let seed: [u8; 32] = pool.hash.finalize().into();
        self.inner = ChaCha20Rng::from_seed(seed);
    }

    /// 32 bytes for the seed file: output of the generator, so the file never holds its state.
    pub fn next_seed_file(&mut self) -> [u8; 32] {
        let mut s = [0u8; 32];
        self.inner.fill_bytes(&mut s);
        s
    }
}

impl RngCore for Rng {
    fn next_u32(&mut self) -> u32 {
        self.inner.next_u32()
    }
    fn next_u64(&mut self) -> u64 {
        self.inner.next_u64()
    }
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        self.inner.fill_bytes(dest)
    }
    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
        self.inner.try_fill_bytes(dest)
    }
}

impl CryptoRng for Rng {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn different_pools_give_different_streams() {
        let mut a = Pool::new();
        a.mix(b"seed one");
        let mut b = Pool::new();
        b.mix(b"seed two");
        assert_ne!(Rng::from_pool(a).next_u64(), Rng::from_pool(b).next_u64());
    }

    #[test]
    fn reseeding_changes_the_stream_and_the_file_is_not_the_state() {
        let mut p = Pool::new();
        p.mix(b"x");
        let mut r = Rng::from_pool(p);
        let mut q = Pool::new();
        q.mix(b"x");
        let mut same = Rng::from_pool(q);
        assert_eq!(r.next_u64(), same.next_u64());
        let mut extra = Pool::new();
        extra.mix_u32(7);
        r.reseed(extra);
        assert_ne!(r.next_u64(), same.next_u64());
        assert_ne!(r.next_seed_file(), r.next_seed_file());
    }
}
