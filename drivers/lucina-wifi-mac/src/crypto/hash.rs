//! SHA-1, SHA-256, HMAC and PBKDF2-HMAC-SHA1.

use super::secret::{wipe, wipe_u32};

/// A hash with a 64-byte block, the only kind used here.
pub trait Digest: Clone {
    const OUT: usize;
    fn new() -> Self;
    fn update(&mut self, data: &[u8]);
    /// Writes `Self::OUT` bytes to the start of `out`.
    fn finish(self, out: &mut [u8]);
}

/// Buffers bytes into 64-byte blocks and pads the last one (both hashes pad alike).
#[derive(Clone)]
struct Blocks {
    buf: [u8; 64],
    used: usize,
    total: u64,
}

impl Blocks {
    const fn new() -> Self {
        Self {
            buf: [0; 64],
            used: 0,
            total: 0,
        }
    }

    fn update(&mut self, mut data: &[u8], mut block: impl FnMut(&[u8; 64])) {
        self.total = self.total.wrapping_add(data.len() as u64);
        if self.used > 0 {
            let take = (64 - self.used).min(data.len());
            self.buf[self.used..self.used + take].copy_from_slice(&data[..take]);
            self.used += take;
            data = &data[take..];
            if self.used < 64 {
                return;
            }
            block(&self.buf);
            self.used = 0;
        }
        while data.len() >= 64 {
            let (head, rest) = data.split_at(64);
            let mut one = [0u8; 64];
            one.copy_from_slice(head);
            block(&one);
            wipe(&mut one);
            data = rest;
        }
        self.buf[..data.len()].copy_from_slice(data);
        self.used = data.len();
    }

    fn pad(&mut self, mut block: impl FnMut(&[u8; 64])) {
        let bits = self.total.wrapping_mul(8);
        self.buf[self.used] = 0x80;
        self.used += 1;
        if self.used > 56 {
            self.buf[self.used..].fill(0);
            block(&self.buf);
            self.used = 0;
        }
        self.buf[self.used..56].fill(0);
        self.buf[56..].copy_from_slice(&bits.to_be_bytes());
        block(&self.buf);
    }
}

impl Drop for Blocks {
    fn drop(&mut self) {
        wipe(&mut self.buf);
    }
}

/// SHA-1. Broken for collisions; HMAC-SHA1, which is all WPA2 uses it for, is not affected.
#[derive(Clone)]
pub struct Sha1 {
    h: [u32; 5],
    blocks: Blocks,
}

fn sha1_block(h: &mut [u32; 5], block: &[u8; 64]) {
    let mut w = [0u32; 80];
    for (i, c) in block.as_chunks::<4>().0.iter().enumerate() {
        w[i] = u32::from_be_bytes([c[0], c[1], c[2], c[3]]);
    }
    for i in 16..80 {
        w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
    }
    let [mut a, mut b, mut c, mut d, mut e] = *h;
    for (i, word) in w.iter().enumerate() {
        let (f, k) = match i {
            0..=19 => ((b & c) | (!b & d), 0x5A82_7999),
            20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
            40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
            _ => (b ^ c ^ d, 0xCA62_C1D6),
        };
        let t = a
            .rotate_left(5)
            .wrapping_add(f)
            .wrapping_add(e)
            .wrapping_add(k)
            .wrapping_add(*word);
        e = d;
        d = c;
        c = b.rotate_left(30);
        b = a;
        a = t;
    }
    h[0] = h[0].wrapping_add(a);
    h[1] = h[1].wrapping_add(b);
    h[2] = h[2].wrapping_add(c);
    h[3] = h[3].wrapping_add(d);
    h[4] = h[4].wrapping_add(e);
    wipe_u32(&mut w);
}

impl Digest for Sha1 {
    const OUT: usize = 20;

    fn new() -> Self {
        Self {
            h: [
                0x6745_2301,
                0xEFCD_AB89,
                0x98BA_DCFE,
                0x1032_5476,
                0xC3D2_E1F0,
            ],
            blocks: Blocks::new(),
        }
    }

    fn update(&mut self, data: &[u8]) {
        let h = &mut self.h;
        self.blocks.update(data, |b| sha1_block(h, b));
    }

    fn finish(mut self, out: &mut [u8]) {
        let h = &mut self.h;
        self.blocks.pad(|b| sha1_block(h, b));
        for (o, w) in out.as_chunks_mut::<4>().0.iter_mut().zip(self.h.iter()) {
            o.copy_from_slice(&w.to_be_bytes());
        }
    }
}

impl Drop for Sha1 {
    fn drop(&mut self) {
        wipe_u32(&mut self.h);
    }
}

const SHA256_K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// SHA-256, for AKM 6's key derivation.
#[derive(Clone)]
pub struct Sha256 {
    h: [u32; 8],
    blocks: Blocks,
}

fn sha256_block(h: &mut [u32; 8], block: &[u8; 64]) {
    let mut w = [0u32; 64];
    for (i, c) in block.as_chunks::<4>().0.iter().enumerate() {
        w[i] = u32::from_be_bytes([c[0], c[1], c[2], c[3]]);
    }
    for i in 16..64 {
        let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
        let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16]
            .wrapping_add(s0)
            .wrapping_add(w[i - 7])
            .wrapping_add(s1);
    }
    let mut v = *h;
    for i in 0..64 {
        let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
        let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
        let t1 = v[7]
            .wrapping_add(s1)
            .wrapping_add(ch)
            .wrapping_add(SHA256_K[i])
            .wrapping_add(w[i]);
        let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
        let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
        let t2 = s0.wrapping_add(maj);
        v[7] = v[6];
        v[6] = v[5];
        v[5] = v[4];
        v[4] = v[3].wrapping_add(t1);
        v[3] = v[2];
        v[2] = v[1];
        v[1] = v[0];
        v[0] = t1.wrapping_add(t2);
    }
    for (x, y) in h.iter_mut().zip(v.iter()) {
        *x = x.wrapping_add(*y);
    }
    wipe_u32(&mut w);
    wipe_u32(&mut v);
}

impl Digest for Sha256 {
    const OUT: usize = 32;

    fn new() -> Self {
        Self {
            h: [
                0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
                0x5be0cd19,
            ],
            blocks: Blocks::new(),
        }
    }

    fn update(&mut self, data: &[u8]) {
        let h = &mut self.h;
        self.blocks.update(data, |b| sha256_block(h, b));
    }

    fn finish(mut self, out: &mut [u8]) {
        let h = &mut self.h;
        self.blocks.pad(|b| sha256_block(h, b));
        for (o, w) in out.as_chunks_mut::<4>().0.iter_mut().zip(self.h.iter()) {
            o.copy_from_slice(&w.to_be_bytes());
        }
    }
}

impl Drop for Sha256 {
    fn drop(&mut self) {
        wipe_u32(&mut self.h);
    }
}

/// HMAC over a 64-byte-block hash. The prepared state can be cloned, which is what makes PBKDF2 affordable.
#[derive(Clone)]
pub struct Hmac<D: Digest> {
    inner: D,
    outer: D,
}

impl<D: Digest> Hmac<D> {
    pub fn new(key: &[u8]) -> Self {
        let mut block = [0u8; 64];
        if key.len() > 64 {
            let mut d = D::new();
            d.update(key);
            d.finish(&mut block);
        } else {
            block[..key.len()].copy_from_slice(key);
        }
        for b in block.iter_mut() {
            *b ^= 0x36;
        }
        let mut inner = D::new();
        inner.update(&block);
        for b in block.iter_mut() {
            *b ^= 0x36 ^ 0x5c;
        }
        let mut outer = D::new();
        outer.update(&block);
        wipe(&mut block);
        Self { inner, outer }
    }

    pub fn update(&mut self, data: &[u8]) {
        self.inner.update(data);
    }

    /// Writes `D::OUT` bytes to the start of `out`.
    pub fn finish(self, out: &mut [u8]) {
        let Self { inner, mut outer } = self;
        let mut tag = [0u8; 32];
        inner.finish(&mut tag);
        outer.update(&tag[..D::OUT]);
        outer.finish(out);
        wipe(&mut tag);
    }
}

/// HMAC-SHA1 of one message.
pub fn hmac_sha1(key: &[u8], data: &[u8]) -> [u8; 20] {
    let mut mac = Hmac::<Sha1>::new(key);
    mac.update(data);
    let mut out = [0; 20];
    mac.finish(&mut out);
    out
}

/// HMAC-SHA256 of one message.
pub fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut mac = Hmac::<Sha256>::new(key);
    mac.update(data);
    let mut out = [0; 32];
    mac.finish(&mut out);
    out
}

/// PBKDF2-HMAC-SHA1 (RFC 8018 section 5.2).
pub fn pbkdf2_hmac_sha1(password: &[u8], salt: &[u8], iterations: u32, out: &mut [u8]) {
    let prepared = Hmac::<Sha1>::new(password);
    for (i, chunk) in out.chunks_mut(20).enumerate() {
        let mut mac = prepared.clone();
        mac.update(salt);
        mac.update(&(i as u32 + 1).to_be_bytes());
        let mut u = [0u8; 20];
        mac.finish(&mut u);
        let mut t = u;
        for _ in 1..iterations {
            let mut mac = prepared.clone();
            mac.update(&u);
            mac.finish(&mut u);
            for (x, y) in t.iter_mut().zip(u.iter()) {
                *x ^= y;
            }
        }
        chunk.copy_from_slice(&t[..chunk.len()]);
        wipe(&mut u);
        wipe(&mut t);
    }
}
