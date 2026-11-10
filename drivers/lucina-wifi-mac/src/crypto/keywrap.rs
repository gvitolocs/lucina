//! AES key wrap (RFC 3394) with a 128-bit KEK: how the AP sends the GTK in EAPOL-Key data.

use super::aes::Aes128;
use super::secret::{SecretVec, ct_eq, wipe};
use alloc::vec;
use alloc::vec::Vec;

const IV: [u8; 8] = [0xA6; 8];

/// Wraps `plain`, whose length must be a multiple of 8 and at least 16. The output is 8 bytes longer.
pub fn aes_wrap(kek: &[u8; 16], plain: &[u8]) -> Option<Vec<u8>> {
    if !plain.len().is_multiple_of(8) || plain.len() < 16 {
        return None;
    }
    let n = plain.len() / 8;
    let aes = Aes128::new(kek);
    let mut out = vec![0u8; plain.len() + 8];
    out[..8].copy_from_slice(&IV);
    out[8..].copy_from_slice(plain);
    let mut block = [0u8; 16];
    for j in 0..6u64 {
        for i in 1..=n {
            block[..8].copy_from_slice(&out[..8]);
            block[8..].copy_from_slice(&out[i * 8..i * 8 + 8]);
            aes.encrypt_block(&mut block);
            let t = (n as u64 * j + i as u64).to_be_bytes();
            for (a, (b, c)) in out[..8].iter_mut().zip(block[..8].iter().zip(t.iter())) {
                *a = b ^ c;
            }
            out[i * 8..i * 8 + 8].copy_from_slice(&block[8..]);
        }
    }
    wipe(&mut block);
    Some(out)
}

/// Unwraps `wrapped`. `None` if the length is wrong or the integrity value does not come out, which is what a
/// wrong KEK or a damaged frame gives.
pub fn aes_unwrap(kek: &[u8; 16], wrapped: &[u8]) -> Option<SecretVec> {
    if !wrapped.len().is_multiple_of(8) || wrapped.len() < 24 {
        return None;
    }
    let n = wrapped.len() / 8 - 1;
    let aes = Aes128::new(kek);
    let mut a = [0u8; 8];
    a.copy_from_slice(&wrapped[..8]);
    let mut work = Vec::from(&wrapped[8..]);
    let mut block = [0u8; 16];
    for j in (0..6u64).rev() {
        for i in (1..=n).rev() {
            let t = (n as u64 * j + i as u64).to_be_bytes();
            for (b, (x, y)) in block[..8].iter_mut().zip(a.iter().zip(t.iter())) {
                *b = x ^ y;
            }
            block[8..].copy_from_slice(&work[(i - 1) * 8..i * 8]);
            aes.decrypt_block(&mut block);
            a.copy_from_slice(&block[..8]);
            work[(i - 1) * 8..i * 8].copy_from_slice(&block[8..]);
        }
    }
    wipe(&mut block);
    // In a SecretVec before the check, so that a failed unwrap wipes what it produced too.
    let plain = SecretVec::new(work);
    ct_eq(&a, &IV).then_some(plain)
}
