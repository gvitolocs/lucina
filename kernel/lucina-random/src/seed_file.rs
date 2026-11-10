//! What the seed file on `/data` holds, and how one seed yields the next and what the kernel gets.
//!
//! The file is 32 bytes, nothing else. Both values are keyed BLAKE2s of the old seed under different labels, the construction
//! systemd-boot uses with SHA-256 (`src/boot/random-seed.c`): seeing one says nothing about the other or about the old seed.
//! What the kernel gets carries no fresh bytes, what goes to the disk does, so that a stolen disk image says nothing about the
//! next boot once the kernel has been seeded by anything else.

use blake2::{Blake2sMac256, digest::Mac};
use zeroize::Zeroizing;

pub const SEED_BYTES: usize = 32;

const LABEL_NEXT: &[u8] = b"lucina random seed: next v1";
const LABEL_MIX: &[u8] = b"lucina random seed: for the kernel v1";

fn keyed(key: &[u8; SEED_BYTES], parts: &[&[u8]]) -> Zeroizing<[u8; SEED_BYTES]> {
    let mut mac = Blake2sMac256::new_from_slice(key).expect("32 bytes is a valid BLAKE2s key");
    for part in parts {
        mac.update(part);
    }
    Zeroizing::new(mac.finalize().into_bytes().into())
}

/// The seed to write back, from the old one and whatever the kernel can give now.
pub fn next_seed(old: &[u8; SEED_BYTES], fresh: &[u8]) -> Zeroizing<[u8; SEED_BYTES]> {
    keyed(old, &[LABEL_NEXT, fresh])
}

/// What to write to `/dev/urandom`.
pub fn for_kernel(old: &[u8; SEED_BYTES]) -> Zeroizing<[u8; SEED_BYTES]> {
    keyed(old, &[LABEL_MIX])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    /// From an independent BLAKE2s (`python3 -c "hashlib.blake2s(msg, key=bytes(range(32)))"`).
    #[test]
    fn known_answers() {
        let old: [u8; 32] = core::array::from_fn(|i| i as u8);
        assert_eq!(hex(&*next_seed(&old, b"fresh")), NEXT_ANSWER);
        assert_eq!(hex(&*for_kernel(&old)), MIX_ANSWER);
    }

    const NEXT_ANSWER: &str = "346a91070d039b8733a3d12976fc430676fe53881e7d5a39888bc0ddf77335ca";
    const MIX_ANSWER: &str = "77221134542e36cb6dcc79db8088c5a2df50e47b42040d8f1e1ebdce2be07bf8";

    #[test]
    fn the_three_values_differ_and_fresh_bytes_change_only_the_next() {
        let old = [7u8; 32];
        let (a, b, m) = (next_seed(&old, b"a"), next_seed(&old, b"b"), for_kernel(&old));
        assert_ne!(*a, *b);
        assert_ne!(*a, *m);
        assert_ne!(*a, old);
        assert_ne!(*m, old);
        assert_eq!(*m, *for_kernel(&old));
    }
}
