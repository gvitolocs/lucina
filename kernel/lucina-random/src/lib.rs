//! Juno's kernel randomness, the parts that need no kernel. The behaviour is Linux 7.1's `drivers/char/random.c`, written
//! again rather than translated (SecretaryIOS `docs/porting-guidelines.md`; GPL, so behaviour only):
//!
//! * [`rules`]: how many bits were credited, what that makes of the generator, who may have output. Proved by Verus.
//! * [`seed_rules`]: the order of the steps with the seed file on `/data`. Proved by Verus.
//! * [`generator`]: the input pool (BLAKE2s, from the `blake2` crate) and the generator (ChaCha20, from the `chacha20`
//!   crate, with fast key erasure). No cryptography is invented here: only the way the two are put together, which is
//!   Linux's.
//! * [`fast_pool`]: the timing of interrupts, on its way to the pool.
//! * [`seed_file`]: what the seed file holds and how the next seed comes from the last one.
//!
//! The kernel's side, the sources and the system calls, is `kernel/core/src/util/random.rs` (patch 0890). Research and the
//! audit that led here: `docs/research/juno-random.md`.
#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

pub mod fast_pool;
pub mod generator;
pub mod rules;
pub mod seed_file;
pub mod seed_rules;
