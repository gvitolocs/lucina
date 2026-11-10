//! Helpers shared by the integration tests: fixture parsing and a deterministic generator.
#![allow(dead_code)]

use std::collections::BTreeMap;

/// `key value...` lines of a fixture file, keyed by the first word.
pub fn kv(text: &str) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    for line in text.lines() {
        if let Some((k, v)) = line.split_once(' ') {
            m.insert(k.to_string(), v.to_string());
        }
    }
    m
}

/// Like [`kv`] but splits at the last space, for keys that contain spaces (`sizeof.struct foo 12`).
pub fn kvl(text: &str) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    for line in text.lines() {
        if let Some((k, v)) = line.rsplit_once(' ') {
            m.insert(k.to_string(), v.to_string());
        }
    }
    m
}

pub fn num(s: &str) -> u64 {
    let s = s.trim();
    if let Some(h) = s.strip_prefix("0x") {
        u64::from_str_radix(h, 16).unwrap()
    } else {
        s.parse::<i64>().unwrap() as u64
    }
}

pub fn hex(s: &str) -> Vec<u8> {
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap())
        .collect()
}

pub fn to_hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// 32-bit Galois LFSR; never yields a stuck state for a non-zero seed.
pub struct Lfsr(pub u32);

impl Lfsr {
    pub fn next(&mut self) -> u32 {
        for _ in 0..8 {
            let lsb = self.0 & 1;
            self.0 >>= 1;
            if lsb != 0 {
                self.0 ^= 0xd000_0001;
            }
        }
        self.0
    }
    pub fn byte(&mut self) -> u8 {
        (self.next() >> 8) as u8
    }
    pub fn fill(&mut self, buf: &mut [u8]) {
        for b in buf.iter_mut() {
            *b = self.byte();
        }
    }
}
