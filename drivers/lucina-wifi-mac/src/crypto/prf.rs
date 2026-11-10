//! The two key derivation functions of IEEE 802.11-2020 section 12.7.1: the SHA-1 PRF and the SHA-256 KDF.

use super::hash::{Hmac, Sha1, Sha256};
use super::secret::wipe;

/// PRF-n(K, A, B): HMAC-SHA1(K, A || 0 || B || i) for i = 0, 1, ..., truncated to `out.len()` bytes.
pub fn prf_sha1(key: &[u8], label: &[u8], data: &[u8], out: &mut [u8]) {
    let prepared = Hmac::<Sha1>::new(key);
    for (i, chunk) in out.chunks_mut(20).enumerate() {
        let mut mac = prepared.clone();
        mac.update(label);
        mac.update(&[0]);
        mac.update(data);
        mac.update(&[i as u8]);
        let mut block = [0u8; 20];
        mac.finish(&mut block);
        chunk.copy_from_slice(&block[..chunk.len()]);
        wipe(&mut block);
    }
}

/// KDF-Hash-Length with SHA-256: HMAC(K, i_le16 || label || context || length_bits_le16), for i = 1, 2, ...
pub fn kdf_sha256(key: &[u8], label: &[u8], context: &[u8], out: &mut [u8]) {
    let prepared = Hmac::<Sha256>::new(key);
    let bits = (out.len() * 8) as u16;
    for (i, chunk) in out.chunks_mut(32).enumerate() {
        let mut mac = prepared.clone();
        mac.update(&(i as u16 + 1).to_le_bytes());
        mac.update(label);
        mac.update(context);
        mac.update(&bits.to_le_bytes());
        let mut block = [0u8; 32];
        mac.finish(&mut block);
        chunk.copy_from_slice(&block[..chunk.len()]);
        wipe(&mut block);
    }
}
