//! The input pool and the generator, after Linux's `random.c`.
//!
//! * The pool is one running BLAKE2s. Taking from it finishes the hash, derives a next key and an output from the result
//!   and restarts the hash under that key (`extract_entropy`), so what came before cannot be worked out afterwards.
//! * The generator is a ChaCha20 key. Each request runs one block under it, keeps the first half of that block as the next
//!   key and hands out the second half; longer requests run further blocks under the old key, which is then wiped
//!   (`crng_fast_key_erasure`, `_get_random_bytes`). So output cannot be worked out from a later state.
//!
//! Linux has one key per CPU to avoid its lock; here one key behind the kernel's lock, held only for the first block.
//! The primitives come from the `blake2` and `chacha20` crates.

use blake2::{Blake2s256, Blake2sMac256, Digest, digest::Mac};
use chacha20::{
    ChaCha20Legacy,
    cipher::{KeyIvInit, StreamCipher},
};
use zeroize::{Zeroize, Zeroizing};

use crate::rules::{self, Credited, Init, Seeding, Then};

/// Four readings of a counter or of anything else that differs: Linux's `RDSEED` block in `extract_entropy`, where ARM has
/// no such instruction and takes `random_get_entropy`. Its worth is not counted.
pub type Noise = [u64; 4];

/// The running hash: plain until the first extraction, as in Linux, then keyed.
enum Hash {
    Plain(Blake2s256),
    Keyed(Blake2sMac256),
}

impl Hash {
    fn update(&mut self, data: &[u8]) {
        match self {
            Hash::Plain(h) => Digest::update(h, data),
            Hash::Keyed(h) => Mac::update(h, data),
        }
    }

    fn finish(self) -> Zeroizing<[u8; 32]> {
        Zeroizing::new(match self {
            Hash::Plain(h) => h.finalize().into(),
            Hash::Keyed(h) => h.finalize().into_bytes().into(),
        })
    }
}

fn keyed(key: &[u8; 32], data: &[u8]) -> Zeroizing<[u8; 32]> {
    let mut mac = Blake2sMac256::new_from_slice(key).expect("32 bytes is a valid BLAKE2s key");
    Mac::update(&mut mac, data);
    Zeroizing::new(mac.finalize().into_bytes().into())
}

/// Blocks of ChaCha20 for one request. Wipes the key when dropped.
pub struct Stream(ChaCha20Legacy);

impl Stream {
    pub fn fill(&mut self, out: &mut [u8]) {
        out.zeroize();
        self.0.apply_keystream(out);
    }
}

/// `input_pool` and `base_crng`.
pub struct Generator {
    hash: Hash,
    seeding: Seeding,
    key: Zeroizing<[u8; 32]>,
    generation: u64,
}

impl Default for Generator {
    fn default() -> Self {
        Self::new()
    }
}

impl Generator {
    pub fn new() -> Self {
        Self {
            hash: Hash::Plain(Blake2s256::new()),
            seeding: rules::start(),
            key: Zeroizing::new([0; 32]),
            generation: 0,
        }
    }

    pub fn init(&self) -> Init {
        self.seeding.init
    }

    pub fn is_ready(&self) -> bool {
        self.seeding.init == Init::Ready
    }

    pub fn credited_bits(&self) -> u32 {
        self.seeding.bits
    }

    /// How many times a key has come from the pool.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// `mix_pool_bytes`.
    pub fn mix(&mut self, data: &[u8]) {
        self.hash.update(data);
    }

    /// `extract_entropy`, for the 32 bytes every caller here wants.
    fn extract(&mut self, noise: &Noise) -> Zeroizing<[u8; 32]> {
        // Whatever stands in for the hash here is replaced below.
        let seed = core::mem::replace(&mut self.hash, Hash::Plain(Blake2s256::new())).finish();
        let block = |counter: u64| {
            let mut b = Zeroizing::new([0u8; 40]);
            for (word, value) in b.chunks_exact_mut(8).zip(noise.iter().chain([&counter])) {
                word.copy_from_slice(&value.to_le_bytes());
            }
            b
        };
        let next_key = keyed(&seed, &*block(0));
        self.hash =
            Hash::Keyed(Blake2sMac256::new_from_slice(&*next_key).expect("32 bytes is a valid BLAKE2s key"));
        keyed(&seed, &*block(1))
    }

    /// `crng_reseed`: a new key from the pool.
    pub fn reseed(&mut self, noise: &Noise) {
        self.key = self.extract(noise);
        self.generation += 1;
    }

    /// `credit_init_bits`. Returns what it did, for the log; a line crossed is seen to here.
    pub fn credit(&mut self, bits: u32, noise: &Noise) -> Credited {
        let credited = rules::credit(self.seeding, bits);
        self.seeding = credited.seeding;
        match credited.then {
            Then::Nothing => {}
            Then::FirstKey | Then::Reseed => self.reseed(noise),
        }
        credited
    }

    /// `crng_make_state`: `first` (up to 32 bytes) gets the second half of a block, the key moves on, and the [`Stream`] goes
    /// on from the next block under the old key. An `Empty` generator takes a key from the pool each time, as in Linux.
    pub fn stream(&mut self, noise: &Noise, first: &mut [u8]) -> Stream {
        assert!(first.len() <= 32);
        if self.seeding.init == Init::Empty {
            self.key = self.extract(noise);
        }
        let mut chacha = ChaCha20Legacy::new((&*self.key).into(), (&[0u8; 8]).into());
        let mut block = Zeroizing::new([0u8; 64]);
        chacha.apply_keystream(&mut *block);
        self.key.copy_from_slice(&block[..32]);
        first.copy_from_slice(&block[32..32 + first.len()]);
        Stream(chacha)
    }

    /// One whole request, for one that has no reason to leave the lock.
    pub fn fill(&mut self, noise: &Noise, out: &mut [u8]) {
        let n = out.len().min(32);
        let (first, rest) = out.split_at_mut(n);
        let mut stream = self.stream(noise, first);
        stream.fill(rest);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOISE: Noise = [1, 2, 3, 4];

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    /// RFC 8439 section 2.3 style vectors, ChaCha20 with a zero key: block 0 and block 1.
    const BLOCK0: &str = "76b8e0ada0f13d90405d6ae55386bd28bdd219b8a08ded1aa836efcc8b770dc7\
                          da41597c5157488d7724e03fb8d84a376a43b8f41518a11cc387b669b2ee6586";
    const BLOCK1: &str = "9f07e7be5551387a98ba977c732d080dcb0f29a048e3656912c6533e32ee7aed\
                          29b721769ce64e43d57133b074d839d531ed1f28510afb45ace10a1f4b794d6f";

    fn at_zero_key() -> Generator {
        let mut g = Generator::new();
        g.seeding = Seeding { bits: 256, init: Init::Ready };
        g
    }

    #[test]
    fn fast_key_erasure_on_known_blocks() {
        let mut g = at_zero_key();
        let mut out = [0u8; 32 + 64];
        g.fill(&NOISE, &mut out);
        // The next key is the first half of block 0, the output its second half and then block 1.
        assert_eq!(hex(&*g.key), BLOCK0[..64]);
        assert_eq!(hex(&out[..32]), BLOCK0[64..]);
        assert_eq!(hex(&out[32..]), BLOCK1);
    }

    #[test]
    fn a_short_request_takes_from_the_block_and_moves_the_key() {
        let mut g = at_zero_key();
        let mut out = [0u8; 5];
        g.fill(&NOISE, &mut out);
        assert_eq!(hex(&out), BLOCK0[64..74]);
        assert_eq!(hex(&*g.key), BLOCK0[..64]);
        let mut again = [0u8; 5];
        g.fill(&NOISE, &mut again);
        assert_ne!(out, again);
    }

    #[test]
    fn output_never_holds_the_next_key() {
        let mut g = at_zero_key();
        let mut out = [0u8; 4096];
        g.fill(&NOISE, &mut out);
        assert!(!out.windows(32).any(|w| w == &g.key[..]));
    }

    /// The pool, with a BLAKE2s from elsewhere (`hashlib.blake2s`), for the same inputs: plain until the first extraction,
    /// then keyed.
    #[test]
    fn extraction_on_known_answers() {
        let mut g = Generator::new();
        g.mix(b"abc");
        let first = g.extract(&NOISE);
        assert_eq!(hex(&*first), EXTRACT_1);
        g.mix(b"def");
        let second = g.extract(&NOISE);
        assert_eq!(hex(&*second), EXTRACT_2);
    }

    const EXTRACT_1: &str = "facdfd274e1917277a2ec3f718c31bdf0a1f94b1155309e75ec021d18958ca99";
    const EXTRACT_2: &str = "bfc8cffd9a263e8e9d9c1b72515ebf56ce445bceba98af9432f81a6064c500d2";

    #[test]
    fn mixing_changes_what_comes_out() {
        let (mut a, mut b) = (Generator::new(), Generator::new());
        a.mix(b"seed one");
        b.mix(b"seed two");
        a.credit(256, &NOISE);
        b.credit(256, &NOISE);
        let (mut x, mut y) = ([0u8; 32], [0u8; 32]);
        a.fill(&NOISE, &mut x);
        b.fill(&NOISE, &mut y);
        assert_ne!(x, y);
    }

    #[test]
    fn a_credit_reseeds_on_the_lines_only() {
        let mut g = Generator::new();
        g.mix(b"x");
        g.credit(100, &NOISE);
        assert_eq!((g.init(), g.generation()), (Init::Empty, 0));
        g.credit(28, &NOISE);
        assert_eq!((g.init(), g.generation()), (Init::Early, 1));
        g.credit(127, &NOISE);
        assert_eq!((g.init(), g.generation()), (Init::Early, 1));
        g.credit(1, &NOISE);
        assert_eq!((g.init(), g.generation()), (Init::Ready, 2));
        assert!(g.is_ready());
        g.credit(256, &NOISE);
        assert_eq!(g.generation(), 2);
    }

    #[test]
    fn an_empty_generator_still_differs_call_to_call() {
        let mut g = Generator::new();
        let (mut x, mut y) = ([0u8; 32], [0u8; 32]);
        g.fill(&NOISE, &mut x);
        g.fill(&NOISE, &mut y);
        assert_ne!(x, y);
    }

    #[test]
    fn a_reseed_changes_the_stream() {
        let mut a = at_zero_key();
        let mut b = at_zero_key();
        b.reseed(&NOISE);
        let (mut x, mut y) = ([0u8; 32], [0u8; 32]);
        a.fill(&NOISE, &mut x);
        b.fill(&NOISE, &mut y);
        assert_ne!(x, y);
    }
}
