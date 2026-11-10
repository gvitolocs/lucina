//! AES-128-CMAC (RFC 4493), the MIC of EAPOL-Key descriptor version 3 (AKM 6).

use super::aes::Aes128;

fn double(b: &[u8; 16]) -> [u8; 16] {
    let mut out = [0u8; 16];
    let mut carry = 0u8;
    for i in (0..16).rev() {
        out[i] = (b[i] << 1) | carry;
        carry = b[i] >> 7;
    }
    if carry != 0 {
        out[15] ^= 0x87;
    }
    out
}

/// AES-128-CMAC of `data`.
pub fn aes128_cmac(key: &[u8; 16], data: &[u8]) -> [u8; 16] {
    let aes = Aes128::new(key);
    let mut l = [0u8; 16];
    aes.encrypt_block(&mut l);
    let k1 = double(&l);
    let k2 = double(&k1);

    let mut x = [0u8; 16];
    let mut rest = data;
    while rest.len() > 16 {
        for (a, b) in x.iter_mut().zip(rest[..16].iter()) {
            *a ^= b;
        }
        aes.encrypt_block(&mut x);
        rest = &rest[16..];
    }
    let mut last = [0u8; 16];
    last[..rest.len()].copy_from_slice(rest);
    if rest.len() == 16 {
        for (a, b) in last.iter_mut().zip(k1.iter()) {
            *a ^= b;
        }
    } else {
        last[rest.len()] = 0x80;
        for (a, b) in last.iter_mut().zip(k2.iter()) {
            *a ^= b;
        }
    }
    for (a, b) in x.iter_mut().zip(last.iter()) {
        *a ^= b;
    }
    aes.encrypt_block(&mut x);
    x
}
